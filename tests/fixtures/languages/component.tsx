export const Café = <T,>({ value }: { value: T }) => (
  <main data-value={value}>λ</main>
);

export interface Props {
  title: string;
}

export function Panel({ title }: Props) {
  return <section><h1>{title}</h1></section>;
}
