export function before() {}

export class Broken {
  okay() {
    return <div>{;</div>;
  }
}
