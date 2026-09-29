export function submitDirectoryImport(control, manifest, files) {
    const job = {...JSON.parse(manifest), files: Array.from(files)};
    return new Promise((resolve, reject) => {
        const channel = new MessageChannel();
        channel.port1.onmessage = ({data}) => {
            channel.port1.close();
            if (data.error) reject(Error(data.error));
            else resolve(JSON.stringify(data.failures));
        };
        control.postMessage({transport: 'import_submit', job, reply: channel.port2}, [channel.port2]);
    });
}
