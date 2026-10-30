"""Protocol-accurate test servers for the Spid3r database clients.

Three servers on ephemeral ports, speaking the real wire protocols:
  - RESP2 (Redis): a working key/value store (SET/GET/INCR/DEL/LPUSH/LRANGE...)
  - PostgreSQL v3: cycles auth methods per connection (trust, cleartext, md5,
    SCRAM-SHA-256) and answers the simple query protocol with a fixed table
  - MySQL protocol 41: mysql_native_password handshake that VERIFIES the
    client's scramble with hashlib, then serves a fixed resultset

These exist so the Rust integration test can prove the clients handle real
handshakes, auth maths and packet framing without needing database servers
installed on the CI runner. Run: python3 tools/db_test_servers.py
It prints one line "READY redis=<port> pg=<port> mysql=<port>" and serves until
killed.
"""

import base64
import hashlib
import hmac
import socket
import struct
import threading

HOST = "127.0.0.1"

# ------------------------------------------------------------------- redis

def serve_redis(conn):
    db = {}
    lists = {}
    f = conn.makefile("rb")
    try:
        while True:
            line = f.readline()
            if not line:
                return
            line = line.rstrip(b"\r\n")
            if line.startswith(b"*"):
                count = int(line[1:])
                args = []
                for _ in range(count):
                    hdr = f.readline().rstrip(b"\r\n")
                    if not hdr.startswith(b"$"):
                        return
                    n = int(hdr[1:])
                    data = f.read(n + 2)[:n] if n >= 0 else b""
                    args.append(data.decode("utf-8", "replace"))
            else:
                args = line.decode("utf-8", "replace").split()
            if not args:
                continue
            cmd = args[0].upper()
            if cmd == "PING":
                conn.sendall(b"+PONG\r\n")
            elif cmd == "AUTH":
                conn.sendall(b"+OK\r\n")
            elif cmd == "ECHO":
                v = args[1].encode()
                conn.sendall(b"$%d\r\n%s\r\n" % (len(v), v))
            elif cmd == "SET":
                db[args[1]] = args[2]
                conn.sendall(b"+OK\r\n")
            elif cmd == "GET":
                v = db.get(args[1])
                if v is None:
                    conn.sendall(b"$-1\r\n")
                else:
                    enc = v.encode()
                    conn.sendall(b"$%d\r\n%s\r\n" % (len(enc), enc))
            elif cmd == "INCR":
                db[args[1]] = str(int(db.get(args[1], "0")) + 1)
                conn.sendall(b":%d\r\n" % int(db[args[1]]))
            elif cmd == "DECR":
                db[args[1]] = str(int(db.get(args[1], "0")) - 1)
                conn.sendall(b":%d\r\n" % int(db[args[1]]))
            elif cmd == "DEL":
                removed = 0
                for k in args[1:]:
                    if db.pop(k, None) is not None:
                        removed += 1
                conn.sendall(b":%d\r\n" % removed)
            elif cmd == "LPUSH":
                lst = lists.setdefault(args[1], [])
                for v in args[2:]:
                    lst.insert(0, v)
                conn.sendall(b":%d\r\n" % len(lst))
            elif cmd == "LRANGE":
                start, stop = int(args[2]), int(args[3])
                lst = lists.get(args[1], [])[start : stop + 1 if stop >= 0 else None]
                out = b"*%d\r\n" % len(lst)
                for v in lst:
                    enc = v.encode()
                    out += b"$%d\r\n%s\r\n" % (len(enc), enc)
                conn.sendall(out)
            elif cmd == "EXISTS":
                conn.sendall(b":%d\r\n" % (1 if args[1] in db or args[1] in lists else 0))
            elif cmd == "FLUSHALL":
                db.clear()
                lists.clear()
                conn.sendall(b"+OK\r\n")
            elif cmd == "QUIT":
                conn.sendall(b"+OK\r\n")
                return
            else:
                msg = ("unknown command '%s'" % cmd).encode()
                conn.sendall(b"-ERR " + msg + b"\r\n")
    except OSError:
        return
    finally:
        conn.close()


# --------------------------------------------------------------- postgres


PG_PASSWORD = "pencil"
PG_SALT = base64.b64decode("W22ZaJ0SNY7soEsUEjb6gQ==")  # RFC 7677 salt
PG_ITERATIONS = 4096


def pg_scram_keys(password):
    salted = hashlib.pbkdf2_hmac("sha256", password.encode(), PG_SALT, PG_ITERATIONS, 32)
    client_key = hmac.new(salted, b"Client Key", hashlib.sha256).digest()
    stored_key = hashlib.sha256(client_key).digest()
    server_key = hmac.new(salted, b"Server Key", hashlib.sha256).digest()
    return stored_key, server_key


PG_STORED_KEY, PG_SERVER_KEY = pg_scram_keys(PG_PASSWORD)

PG_COLUMNS = [
    ("id", 23),      # int4
    ("name", 25),    # text
    ("price", 1700), # numeric
]
PG_ROWS = [
    [b"1", b"ada", b"9.99"],
    [b"2", b"bob", None],
]


def pg_send(conn, kind, payload):
    conn.sendall(kind + struct.pack(">I", len(payload) + 4) + payload)


def pg_read_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise OSError("closed")
        buf += chunk
    return buf


def serve_pg(conn):
    try:
        # StartupMessage: int32 len, int32 protocol, then NUL-separated pairs.
        raw = pg_read_exact(conn, 4)
        (length,) = struct.unpack(">I", raw)
        body = pg_read_exact(conn, length - 4)
        (proto,) = struct.unpack(">I", body[:4])
        if proto != 196608:
            pg_send(conn, b"E", b"SFATAL\0C0A000\0Munsupported protocol version\0\0")
            return

        # The username picks the auth method, so each test connection is
        # independent (no hidden server-side state).
        method = "trust"
        parts = body[4:].split(b"\0")
        for i in range(0, len(parts) - 1, 2):
            if parts[i] == b"user":
                user_name = parts[i + 1].decode()
                method = {
                    "ada": "trust",
                    "ada_clear": "cleartext",
                    "ada_md5": "md5",
                    "ada_scram": "scram",
                }.get(user_name, "trust")
                break

        if method == "cleartext":
            pg_send(conn, b"R", struct.pack(">I", 3))
            data = pg_read_exact(conn, 5)
            (plen,) = struct.unpack(">I", data[1:5])
            answer = pg_read_exact(conn, plen - 4).rstrip(b"\0").decode()
            if answer != PG_PASSWORD:
                pg_send(conn, b"E", b"SFATAL\0C28P01\0Mpassword authentication failed\0\0")
                return
        elif method == "md5":
            pg_send(conn, b"R", struct.pack(">I", 5) + bytes([1, 2, 3, 4]))
            data = pg_read_exact(conn, 5)
            (plen,) = struct.unpack(">I", data[1:5])
            answer = pg_read_exact(conn, plen - 4).rstrip(b"\0").decode()
            inner = hashlib.md5((PG_PASSWORD + user_name).encode()).hexdigest()
            expected = "md5" + hashlib.md5(inner.encode() + bytes([1, 2, 3, 4])).hexdigest()
            if answer != expected:
                pg_send(conn, b"E", b"SFATAL\0C28P01\0Mpassword authentication failed\0\0")
                return
        elif method == "scram":
            pg_send(conn, b"R", struct.pack(">I", 10) + b"SCRAM-SHA-256\0")
            data = pg_read_exact(conn, 5)
            (plen,) = struct.unpack(">I", data[1:5])
            init = pg_read_exact(conn, plen - 4)
            # mechanism NUL, then int32 length, then client-first-message
            mech_end = init.index(b"\0")
            (bare_len,) = struct.unpack(">I", init[mech_end + 1 : mech_end + 5])
            client_first = init[mech_end + 5 : mech_end + 5 + bare_len].decode()
            client_nonce = client_first.split("r=")[1].split(",")[0]
            server_nonce = "hNlF$k0extra"
            server_first = "r=%s%s,s=%s,i=%d" % (
                client_nonce,
                server_nonce,
                base64.b64encode(PG_SALT).decode(),
                PG_ITERATIONS,
            )
            pg_send(conn, b"R", struct.pack(">I", 11) + server_first.encode())

            data = pg_read_exact(conn, 5)
            (plen,) = struct.unpack(">I", data[1:5])
            final_msg = pg_read_exact(conn, plen - 4).rstrip(b"\0").decode()
            # SCRAM attributes are key=value separated by commas, but base64
            # values may themselves contain '=' padding, so split on the known
            # keys rather than on every '='.
            attrs = {}
            for key in ("c", "r", "p"):
                marker = key + "="
                pos = final_msg.find(marker)
                if pos >= 0:
                    start = pos + len(marker)
                    end = final_msg.find(",", start)
                    attrs[key] = final_msg[start:] if end < 0 else final_msg[start:end]
            proof = base64.b64decode(attrs["p"])
            client_final_bare = "c=biws,r=" + attrs["r"]
            auth_message = ",".join([client_first, server_first, client_final_bare])
            client_sig = hmac.new(PG_STORED_KEY, auth_message.encode(), hashlib.sha256).digest()
            client_key = bytes(a ^ b for a, b in zip(proof, client_sig))
            if hashlib.sha256(client_key).digest() != PG_STORED_KEY:
                pg_send(conn, b"E", b"SFATAL\0C28P01\0MSCRAM proof mismatch\0\0")
                return
            server_sig = hmac.new(PG_SERVER_KEY, auth_message.encode(), hashlib.sha256).digest()
            pg_send(
                conn,
                b"R",
                struct.pack(">I", 12) + ("v=" + base64.b64encode(server_sig).decode()).encode(),
            )

        pg_send(conn, b"R", struct.pack(">I", 0))  # AuthenticationOk
        pg_send(conn, b"S", b"server_version\017.0\0")
        pg_send(conn, b"K", struct.pack(">II", 1234, 5678))
        pg_send(conn, b"Z", b"I")

        while True:
            head = pg_read_exact(conn, 5)
            kind = head[0:1]
            (length,) = struct.unpack(">I", head[1:5])
            payload = pg_read_exact(conn, length - 4)
            if kind == b"Q":
                # RowDescription for the fixed table
                desc = struct.pack(">H", len(PG_COLUMNS))
                for name, oid in PG_COLUMNS:
                    desc += name.encode() + b"\0"
                    desc += struct.pack(">IhIhih", 0, 0, oid, -1, -1, 0)
                pg_send(conn, b"T", desc)
                for row in PG_ROWS:
                    body = struct.pack(">H", len(row))
                    for v in row:
                        if v is None:
                            body += struct.pack(">i", -1)  # NULL
                        else:
                            body += struct.pack(">I", len(v)) + v
                    pg_send(conn, b"D", body)
                pg_send(conn, b"C", ("SELECT %d" % len(PG_ROWS)).encode())
                pg_send(conn, b"Z", b"I")
            elif kind == b"X":
                return
            elif kind == b"p":
                continue  # stray auth response, ignore
            else:
                pg_send(conn, b"E", b"SFATAL\0C08P01\0Munexpected message\0\0")
                return
    except OSError:
        return
    finally:
        conn.close()


# ------------------------------------------------------------------ mysql

MYSQL_PASSWORD = "secret123"
MYSQL_NONCE = bytes(range(1, 21))  # 20 bytes, part1 (8) + part2 (12)


def mysql_scramble(password, nonce):
    h1 = hashlib.sha1(password.encode()).digest()
    h2 = hashlib.sha1(h1).digest()
    h3 = hashlib.sha1(nonce + h2).digest()
    return bytes(a ^ b for a, b in zip(h1, h3))


MYSQL_COLUMNS = [
    ("id", 3),      # LONG -> int
    ("name", 253),  # VARCHAR -> string
    ("price", 246), # NEWDECIMAL -> float
]
MYSQL_ROWS = [
    [b"1", b"ada", b"9.99"],
    [b"2", b"bob", None],  # NULL price
]


def mysql_packet(payload, seq):
    return struct.pack("<I", len(payload))[:3] + bytes([seq]) + payload


def serve_mysql(conn):
    seq = 0
    try:
        # Handshake v10
        hs = bytes([10]) + b"8.0.0-spid3r-test\0"
        hs += struct.pack("<I", 7)            # connection id
        hs += MYSQL_NONCE[:8]                  # auth-plugin-data-part-1
        hs += b"\0"                            # filler
        caps = 0x00000000
        caps |= 1 << 9                         # CLIENT_PROTOCOL_41
        caps |= 1 << 19                        # CLIENT_PLUGIN_AUTH
        caps |= 1 << 15                        # CLIENT_SECURE_CONNECTION
        hs += struct.pack("<H", caps & 0xFFFF)
        hs += bytes([45])                      # charset utf8mb4
        hs += struct.pack("<H", 2)             # status flags
        hs += struct.pack("<H", (caps >> 16) & 0xFFFF)
        hs += bytes([8 + 13])                  # auth data length
        hs += b"\0" * 10                       # reserved
        hs += MYSQL_NONCE[8:20] + b"\0"        # part 2 (12 + NUL)
        hs += b"mysql_native_password\0"
        conn.sendall(mysql_packet(hs, 0))
        seq = 1

        head = pg_read_exact(conn, 4)
        (length,) = struct.unpack("<I", head[:3] + b"\0")
        payload = pg_read_exact(conn, length)
        seq = head[3] + 1

        # HandshakeResponse41: caps(4) max(4) charset(1) reserved(23)
        # user\0 lenenc-auth-response db\0 plugin\0
        at = 32
        end = payload.index(b"\0", at)
        user = payload[at:end].decode()
        at = end + 1
        resp_len = payload[at]
        client_response = payload[at + 1 : at + 1 + resp_len]
        # skip db and plugin; not validated here
        if client_response != mysql_scramble(MYSQL_PASSWORD, MYSQL_NONCE):
            conn.sendall(
                mysql_packet(b"\xff\x15\x04#28000Access denied (scramble mismatch)", 2)
            )
            return
        conn.sendall(mysql_packet(b"\x00\x00\x00\x02\x00\x00\x00", 2))  # OK
        seq = 3

        while True:
            head = pg_read_exact(conn, 4)
            (length,) = struct.unpack("<I", head[:3] + b"\0")
            payload = pg_read_exact(conn, length)
            cmd = payload[0]
            if cmd == 0x01:  # COM_QUIT
                return
            if cmd != 0x03:  # COM_QUERY
                conn.sendall(mysql_packet(b"\xff\x2a\x00#42000unknown command", 1))
                return
            # The client's command packet used sequence 0, so the response
            # starts at 1 and counts up from there.
            out_seq = 1
            # Resultset: column count, definitions, EOF, rows, EOF
            out = mysql_packet(b"\x03", out_seq)
            out_seq += 1
            for name, type_code in MYSQL_COLUMNS:
                # catalog, schema, table, org_table, name, org_name — each is a
                # length-encoded string, so an empty one is a single 0x00 byte.
                # catalog="def", then schema/table/org_table/org_name are empty
                # (each a single 0x00 length byte). A length-encoded string
                # carries no NUL terminator, so there is none after "def".
                body = (
                    bytes([3]) + b"def"
                    + b"\x00" * 3
                    + bytes([len(name)]) + name.encode()
                    + b"\x00"
                )
                body += b"\x0c"
                body += struct.pack("<H", 45)   # charset
                body += struct.pack("<I", 256)  # column length
                body += bytes([type_code])
                body += struct.pack("<H", 0)    # flags
                body += bytes([0])              # decimals
                body += b"\0\0"                 # filler
                out += mysql_packet(body, out_seq)
                out_seq += 1
            out += mysql_packet(b"\xfe\x00\x00\x02\x00", out_seq)
            out_seq += 1
            for i, row in enumerate(MYSQL_ROWS):
                body = b""
                for v in row:
                    if v is None:
                        body += b"\xfb"
                    else:
                        body += bytes([len(v)]) + v
                out += mysql_packet(body, out_seq)
                out_seq += 1
            out += mysql_packet(b"\xfe\x00\x00\x02\x00", out_seq)
            conn.sendall(out)
    except OSError:
        return
    finally:
        conn.close()


def main():
    servers = [
        ("redis", serve_redis),
        ("pg", serve_pg),
        ("mysql", serve_mysql),
    ]
    ports = {}
    threads = []
    for name, handler in servers:
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((HOST, 0))
        listener.listen(16)
        port = listener.getsockname()[1]
        ports[name] = port
        t = threading.Thread(
            target=_accept_loop, args=(listener, handler, name), daemon=True
        )
        t.start()
        threads.append(t)
    print("READY " + " ".join("%s=%d" % (k, v) for k, v in ports.items()), flush=True)
    for t in threads:
        t.join()


def _accept_loop(listener, handler, name):
    while True:
        try:
            conn, _addr = listener.accept()
        except OSError:
            return
        threading.Thread(target=handler, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    main()
