"""Self-test for db_test_servers.py: connect to each server with plain
sockets/hashlib and verify the protocol flows work end to end. Run:
python tools/check_db_servers.py  (expects the servers on the printed ports)
"""

import base64
import hashlib
import hmac
import socket
import struct
import subprocess
import sys

HOST = "127.0.0.1"


def read_line(sock):
    buf = b""
    while not buf.endswith(b"\r\n"):
        c = sock.recv(1)
        if not c:
            raise OSError("closed")
        buf += c
    return buf


def read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        c = sock.recv(n - len(buf))
        if not c:
            raise OSError("closed")
        buf += c
    return buf


def read_pg(sock):
    head = read_exact(sock, 5)
    kind = head[0:1]
    (length,) = struct.unpack(">I", head[1:5])
    return kind, read_exact(sock, length - 4)


def test_redis(port):
    s = socket.create_connection((HOST, port), 3)
    s.sendall(b"*1\r\n$4\r\nPING\r\n")
    assert read_line(s) == b"+PONG\r\n"
    s.sendall(b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$4\r\nv 42\r\n")
    assert read_line(s) == b"+OK\r\n"
    s.sendall(b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n")
    assert read_line(s) == b"$4\r\n"
    assert read_exact(s, 6) == b"v 42\r\n"
    s.sendall(b"*2\r\n$4\r\nINCR\r\n$3\r\nctr\r\n")
    got_incr = read_line(s)
    print("INCR reply:", repr(got_incr))
    assert got_incr == b":1\r\n", repr(got_incr)
    s.sendall(b"*2\r\n$4\r\nINCR\r\n$3\r\nctr\r\n")
    assert read_line(s) == b":2\r\n"
    s.sendall(b"*1\r\n$4\r\nQUIT\r\n")
    assert read_line(s) == b"+OK\r\n"
    s.close()


PG_USERS = ["ada", "ada_clear", "ada_md5", "ada_scram"]


def pg_authenticate(port, method_index, password):
    s = socket.create_connection((HOST, port), 3)
    user = PG_USERS[method_index]
    body = struct.pack(">I", 196608) + ("user\0%s\0database\0shop\0\0" % user).encode()
    s.sendall(struct.pack(">I", len(body) + 4) + body)
    kind, payload = read_pg(s)
    if method_index == 0:  # trust
        assert kind == b"R" and payload[:4] == struct.pack(">I", 0)
    elif method_index == 1:  # cleartext
        assert payload[:4] == struct.pack(">I", 3)
        s.sendall(b"p" + struct.pack(">I", len(password) + 5) + password.encode() + b"\0")
    elif method_index == 2:  # md5
        assert payload[:4] == struct.pack(">I", 5), payload[:4]
        salt = payload[4:8]
        inner = hashlib.md5((password + user).encode()).hexdigest()
        answer = "md5" + hashlib.md5(inner.encode() + salt).hexdigest()
        s.sendall(b"p" + struct.pack(">I", len(answer) + 5) + answer.encode() + b"\0")
    else:  # scram-sha-256
        assert payload[:4] == struct.pack(">I", 10), payload[:4]
        nonce = "c2FsdA==extra"
        client_first = "n=,r=" + nonce
        mech = b"SCRAM-SHA-256\0"
        # The frame length counts the 4-byte length field itself.
        frame_len = 4 + len(mech) + 4 + len(client_first)
        s.sendall(
            b"p" + struct.pack(">I", frame_len) + mech
            + struct.pack(">I", len(client_first)) + client_first.encode()
        )
        kind, cont = read_pg(s)
        assert cont[:4] == struct.pack(">I", 11), cont[:4]
        server_first = cont[4:].decode()
        attrs = dict(p.split("=", 1) for p in server_first.split(",") if "=" in p)
        salt = base64.b64decode(attrs["s"])
        iterations = int(attrs["i"])
        client_final_bare = "c=biws,r=" + attrs["r"]
        auth_message = ",".join([client_first, server_first, client_final_bare])
        salted = hashlib.pbkdf2_hmac("sha256", password.encode(), salt, iterations, 32)
        client_key = hmac.new(salted, b"Client Key", hashlib.sha256).digest()
        stored_key = hashlib.sha256(client_key).digest()
        client_sig = hmac.new(stored_key, auth_message.encode(), hashlib.sha256).digest()
        proof = bytes(a ^ b for a, b in zip(client_key, client_sig))
        final = client_final_bare + ",p=" + base64.b64encode(proof).decode()
        s.sendall(b"p" + struct.pack(">I", len(final) + 5) + final.encode() + b"\0")
        kind, fin = read_pg(s)
        assert fin[:4] == struct.pack(">I", 12), fin[:4]
        server_sig = hmac.new(
            hmac.new(salted, b"Server Key", hashlib.sha256).digest(),
            auth_message.encode(),
            hashlib.sha256,
        ).digest()
        assert base64.b64decode(fin[4:].decode().split("v=")[1]) == server_sig
    # Drain until ReadyForQuery.
    while True:
        kind, _ = read_pg(s)
        if kind == b"Z":
            break
    return s


def test_pg(port):
    for i in [0, 1, 2, 3]:
        s = pg_authenticate(port, i, "pencil")
        sql = "SELECT id, name, price FROM items"
        s.sendall(b"Q" + struct.pack(">I", len(sql) + 5) + sql.encode() + b"\0")
        cols = rows = None
        while True:
            kind, payload = read_pg(s)
            if kind == b"Z":
                break
            if kind == b"T":
                (n,) = struct.unpack(">H", payload[:2])
                cols = n
            elif kind == b"D":
                (n,) = struct.unpack(">H", payload[:2])
                assert n == 3
                at = 2
                vals = []
                for _ in range(n):
                    (ln,) = struct.unpack(">I", payload[at : at + 4])
                    at += 4
                    if ln == 0xFFFFFFFF:
                        vals.append(None)
                    else:
                        vals.append(payload[at : at + ln])
                        at += ln
                print("DataRow:", vals)
                assert vals in ([b"1", b"ada", b"9.99"], [b"2", b"bob", None]), vals
                rows = (rows or 0) + 1
        assert cols == 3 and rows == 2
        s.sendall(b"X\0\0\0\x04")
        s.close()


def mysql_native(password, nonce):
    h1 = hashlib.sha1(password.encode()).digest()
    h2 = hashlib.sha1(h1).digest()
    h3 = hashlib.sha1(nonce + h2).digest()
    return bytes(a ^ b for a, b in zip(h1, h3))


def test_mysql(port):
    s = socket.create_connection((HOST, port), 3)
    head = read_exact(s, 4)
    (length,) = struct.unpack("<I", head[:3] + b"\0")
    hs = read_exact(s, length)
    assert hs[0] == 10
    # Handshake v10: [0]=10, server version\0, connid(4), part1(8), filler(1),
    # caps-lower(2), charset(1), status(2), caps-upper(2), auth-len(1),
    # reserved(10), part2(12), plugin\0
    at = 1
    end = hs.index(b"\0", at)
    at = end + 1 + 4
    nonce = hs[at : at + 8]
    at += 8 + 1
    at += 2 + 1 + 2 + 2
    at += 1 + 10
    nonce += hs[at : at + 12]
    nonce = nonce[:20]
    caps = 1 | (1 << 9) | (1 << 13) | (1 << 15) | (1 << 3) | (1 << 19)
    resp = struct.pack("<IIB", caps, 0x1000000, 45) + b"\0" * 23
    resp += b"ada\0"
    sc = mysql_native("secret123", nonce)
    resp += bytes([len(sc)]) + sc
    resp += b"shop\0" + b"mysql_native_password\0"
    s.sendall(struct.pack("<I", len(resp))[:3] + bytes([1]) + resp)

    # The server sends the OK packet with payload after the 4-byte header.
    ok_head = read_exact(s, 4)
    (ok_len,) = struct.unpack("<I", ok_head[:3] + b"\0")
    ok = read_exact(s, ok_len)
    assert ok_head[3] == 2 and ok[:1] == b"\x00", (ok_head, ok)  # OK on seq 2
    sql = "SELECT id, name, price FROM items"
    # Every command restarts the sequence at 0.
    # Payload is the command byte plus the SQL text; the length counts it.
    s.sendall(
        struct.pack("<I", len(sql) + 1)[:3] + bytes([0]) + b"\x03" + sql.encode()
    )
    head = read_exact(s, 4)
    first = read_exact(s, struct.unpack("<I", head[:3] + b"\0")[0])
    (ncols,) = struct.unpack("<B", first[:1])
    assert ncols == 3
    for _ in range(ncols):
        h = read_exact(s, 4)
        read_exact(s, struct.unpack("<I", h[:3] + b"\0")[0])
    h = read_exact(s, 4)
    assert read_exact(s, struct.unpack("<I", h[:3] + b"\0")[0])[:1] == b"\xfe"
    got = []
    while True:
        h = read_exact(s, 4)
        body = read_exact(s, struct.unpack("<I", h[:3] + b"\0")[0])
        if body[:1] == b"\xfe" and len(body) < 9:
            break
        at = 0
        vals = []
        for _ in range(3):
            if body[at : at + 1] == b"\xfb":
                vals.append(None)
                at += 1
            else:
                ln = body[at]
                vals.append(body[at + 1 : at + 1 + ln])
                at += 1 + ln
        got.append(vals)
    assert got == [[b"1", b"ada", b"9.99"], [b"2", b"bob", None]], got
    s.close()


def _accept_dbg(listener, handler):
    while True:
        try:
            conn, _addr = listener.accept()
        except OSError as e:
            print("ACCEPT LOOP DIED:", repr(e), flush=True)
            return
        try:
            handler(conn)
        except Exception:
            import traceback
            traceback.print_exc()
        finally:
            try:
                conn.close()
            except OSError:
                pass


def main():
    import db_test_servers as servers
    import threading
    ports = {}
    threads = []
    for name, handler in [
        ("redis", servers.serve_redis),
        ("pg", servers.serve_pg),
        ("mysql", servers.serve_mysql),
    ]:
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((HOST, 0))
        listener.listen(16)
        ports[name] = listener.getsockname()[1]
        t = threading.Thread(
            target=lambda l=listener, h=handler: _accept_dbg(l, h), daemon=True
        )
        t.start()
        threads.append(t)
    proc = None
    parts = dict(ports)
    try:
        test_redis(int(parts["redis"]))
        print("redis OK")
        test_pg(int(parts["pg"]))
        print("pg OK (trust, cleartext, md5, scram-sha-256)")
        test_mysql(int(parts["mysql"]))
        print("mysql OK (native_password scramble verified by the server)")
    finally:
        pass
    print("ALL SERVER TESTS PASSED")


if __name__ == "__main__":
    main()
