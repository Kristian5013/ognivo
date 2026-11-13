// A fixture of the core's stream, which the gallery's build reads as text.
declare module "*.jsonl" {
  const text: string;
  export default text;
}
