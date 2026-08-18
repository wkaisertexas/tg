struct Outer {
    struct Nested {
        int value;
    } nested;
};

int duplicate(void) { return 1; }
int duplicate(void) { return 2; }

int wrapper(void) {
    int local = 0;
    return local;
}
