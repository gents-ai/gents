export function gateManifest() {
    const prototype = FileSystemDirectoryHandle.prototype;
    const original = prototype.getFileHandle;
    let signalPaused, resume, signalClosed;
    const paused = new Promise(resolve => { signalPaused = resolve; });
    const held = new Promise(resolve => { resume = resolve; });
    const closed = new Promise(resolve => { signalClosed = resolve; });
    const gate = { paused, closed, resume, laterCalls: 0, armed: false,
        blocked: false, released: false };
    prototype.getFileHandle = function (name, options) {
        if (!gate.armed && name.endsWith("%2FMANIFEST")) {
            gate.armed = true;
            return original.call(this, name, options).then(handle => new Proxy(handle, {
                get(target, property) {
                    if (property === "createWritable") return async (...args) => {
                        const stream = await target.createWritable(...args);
                        const close = stream.close.bind(stream);
                        stream.close = () => {
                            gate.blocked = true;
                            signalPaused();
                            return held.then(close).finally(signalClosed);
                        };
                        return stream;
                    };
                    const value = Reflect.get(target, property, target);
                    return typeof value === "function" ? value.bind(target) : value;
                }
            }));
        }
        if (gate.blocked && !gate.released) gate.laterCalls++;
        return original.call(this, name, options);
    };
    gate.restore = () => { prototype.getFileHandle = original; release(gate); };
    return gate;
}

export function manifestPaused(gate) { return gate.paused; }
export function manifestClosed(gate) { return gate.closed; }
export function laterCalls(gate) { return gate.laterCalls; }
export function release(gate) { gate.released = true; gate.resume(); }
export function restore(gate) { gate.restore(); }

// Give every already queued Rust task a poll, without waiting for device I/O.
export async function drainMicrotasks() {
    for (let i = 0; i < 8; i++) await Promise.resolve();
}

export function failRemovals() {
    const prototype = FileSystemDirectoryHandle.prototype;
    const original = prototype.removeEntry;
    const fault = { enabled: true, attempts: 0 };
    prototype.removeEntry = function (name, options) {
        if (name.endsWith("%2Fobsolete")) {
            fault.attempts++;
            if (fault.enabled) {
                return Promise.reject(new DOMException("injected removal failure", "NotAllowedError"));
            }
        }
        return original.call(this, name, options);
    };
    fault.restore = () => { prototype.removeEntry = original; };
    return fault;
}

export function allowRemovals(fault) { fault.enabled = false; }
export function removalAttempts(fault) { return fault.attempts; }

export async function mirrorFileExists(database, path) {
    const root = await navigator.storage.getDirectory();
    const directory = await root.getDirectoryHandle(database);
    const name = ".regolith-file-" + path.replaceAll("%", "%25")
        .replaceAll("/", "%2F").replaceAll("\\", "%5C");
    try {
        await directory.getFileHandle(name);
        return true;
    } catch (error) {
        if (error.name === "NotFoundError") return false;
        throw error;
    }
}
