export function render(value) {
  const nested = () => value;
  return nested();
}

export function* stream() {
  yield 1;
}

export class View {
  #cache = 1;
  title = "example";

  constructor() {}

  render() {
    const local = () => null;
    return local();
  }

  #reset() {}
}

export const build = () => new View();

(function namedExpression() {
  const hidden = () => null;
})();
