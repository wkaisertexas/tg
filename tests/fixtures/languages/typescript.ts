export namespace Api {
  export function request(): void {}

  export interface Client {
    endpoint: string;
    fetch(): Promise<void>;
  }
}

export type Result<T> = { value: T };

export enum State {
  Ready,
  Named = 2,
}

export abstract class Repository {
  abstract load(): void;
  save(): void {}
}

export function parse(value: string): string;
export function parse(value: number): number;
export function parse(value: string | number): string | number {
  return value;
}
