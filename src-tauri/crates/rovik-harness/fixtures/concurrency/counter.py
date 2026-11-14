import threading
import time


class Counter:
    def __init__(self):
        self.lock = threading.Lock()
        self.value = 0
        self.cancelled = threading.Event()
        self.threads = []

    def increment(self):
        with self.lock:
            previous = self.value
            # Deliberately widen the relevant schedule in this conformance fixture.
            time.sleep(0.005)
            self.value = previous + 1

    def start(self):
        def work():
            while not self.cancelled.wait(0.005):
                self.increment()
        thread = threading.Thread(target=work)
        self.threads.append(thread)
        thread.start()

    def cancel(self):
        self.cancelled.set()

    def shutdown(self):
        self.cancel()
        for thread in self.threads:
            thread.join(timeout=2)
        return not any(thread.is_alive() for thread in self.threads)
