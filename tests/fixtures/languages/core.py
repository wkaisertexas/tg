class Outer:
    class Nested:
        def render(self):
            return 1


def duplicate():
    return 1


def duplicate():
    return 2


def wrapper():
    local = 0

    class Hidden:
        pass

    def inner():
        return local

    return inner()
