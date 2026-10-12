// quietRunner.ts
//
// A task run once things settle: `quietMs` after the last trigger. A
// trigger while the task runs queues one more run after it, so nothing
// that happened during a run is missed.
export class QuietRunner {
  private timer: ReturnType<typeof setTimeout> | undefined;
  private running = false;
  private again = false;

  constructor(
    private readonly task: () => Promise<void>,
    private readonly quietMs: number
  ) {}

  trigger(): void {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.run(), this.quietMs);
  }

  dispose(): void {
    clearTimeout(this.timer);
    this.again = false;
  }

  private async run(): Promise<void> {
    if (this.running) {
      this.again = true;
      return;
    }
    this.running = true;
    try {
      await this.task();
    } finally {
      this.running = false;
      if (this.again) {
        this.again = false;
        this.trigger();
      }
    }
  }
}
