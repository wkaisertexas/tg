namespace Outer {
class Nested {
public:
    void render();
};
}

int duplicate() { return 1; }
int duplicate() { return 2; }

int wrapper() {
    int local = 0;
    return local;
}
