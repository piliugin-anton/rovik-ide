import threading
from counter import Counter


def together(counter, count):
    barrier = threading.Barrier(count)
    def increment():
        barrier.wait(timeout=2)
        counter.increment()
    threads = [threading.Thread(target=increment) for _ in range(count)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=2)
    assert not any(thread.is_alive() for thread in threads)


def test_schedules():
    counter = Counter()
    for _ in range(4):
        together(counter, 3)
    assert counter.value == 12


def test_linearizability():
    counter = Counter()
    together(counter, 8)
    assert counter.value == 8


def test_cancellation():
    counter = Counter()
    counter.start()
    counter.cancel()
    try:
        for thread in counter.threads:
            thread.join(timeout=2)
        assert counter.cancelled.is_set()
        assert not any(thread.is_alive() for thread in counter.threads)
    finally:
        counter.shutdown()


def test_shutdown():
    counter = Counter()
    for _ in range(3):
        counter.start()
    assert counter.shutdown()
    assert not any(thread.is_alive() for thread in counter.threads)
