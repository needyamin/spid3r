// R2.4 Node.js http_echo baseline server (matching 64-byte body).
const http = require("http");
const BODY = Buffer.from("x".repeat(64));

const server = http.createServer((req, res) => {
  const close = (req.headers.connection || "").toLowerCase() === "close";
  res.writeHead(200, {
    "Content-Length": BODY.length,
    "Connection": close ? "close" : "keep-alive",
  });
  res.end(BODY);
});

server.keepAliveTimeout = 60000;
server.listen(0, "127.0.0.1", () => {
  const a = server.address();
  console.log(`READY 127.0.0.1:${a.port}`);
});
