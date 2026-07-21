const MAX_BUFFER_CHARS = 8_000;
const TAIL_CHARS = 4_000;

export type RunnerExitStatus = {
  code: number | null;
  signal: NodeJS.Signals | null;
};

class LogChunkBuffer {
  private readonly chunks: string[] = [];

  push(chunk: string) {
    if (!chunk) {
      return;
    }
    this.chunks.push(chunk);
    while (this.chunks.join("").length > MAX_BUFFER_CHARS) {
      this.chunks.shift();
    }
  }

  tail() {
    return this.chunks.join("").slice(-TAIL_CHARS);
  }
}

export class RunnerLogs {
  private readonly stdout = new LogChunkBuffer();
  private readonly stderr = new LogChunkBuffer();

  pushStdout(chunk: string) {
    this.stdout.push(chunk);
  }

  pushStderr(chunk: string) {
    this.stderr.push(chunk);
  }

  stdoutTail() {
    return this.stdout.tail();
  }

  stderrTail() {
    return this.stderr.tail();
  }
}
