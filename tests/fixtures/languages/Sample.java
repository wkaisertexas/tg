package fixtures;

class Service {
    String name, alias;

    Service() {}

    void render() {
        int local = 1;
        class Hidden {}
    }

    private void save() {}

    class Inner {
        void render() {}
    }
}

interface Reader {
    String read();

    default void close() {}
}

enum State {
    READY,
    NAMED(2);

    State() {}
    State(int value) {}
}

record Result<T>(T value) {
    T unwrap() { return value; }
}

@interface Marker {
    String value();
}

class Café {
    String crème;
}
