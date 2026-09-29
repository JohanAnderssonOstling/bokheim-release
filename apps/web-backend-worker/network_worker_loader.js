import initialize, {startNetworkWorker} from "./sync_coordinator.js";
const waiting = [];
self.onmessage = ({data}) => waiting.push(data);
await initialize();
startNetworkWorker();
for (const data of waiting) self.onmessage({data});
waiting.length = 0;
