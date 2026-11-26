const { Worker, isMainThread, parentPort, workerData } = require("worker_threads");

const N = 2000;
if (isMainThread) {
  const w = new Worker(__filename, { workerData: N });
  let total = 0;
  let received = 0;
  w.on("message", (v) => {
    total += v;
    received += 1;
    if (received === N) {
      console.log(total);
    }
  });
} else {
  for (let i = 0; i < workerData; i++) {
    parentPort.postMessage(i * 2);
  }
}
