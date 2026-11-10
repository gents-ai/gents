function trimFixed(value: number, digits: number) {
  return value.toFixed(digits).replace(/\.0$/, "");
}

/** a token count written short: 950, 18.4k, 182k, 1.2M */
export function compactTokens(tokens: number) {
  if (tokens < 1000) return tokens.toLocaleString();
  const thousands = tokens / 1000;
  if (thousands < 999.5) return `${trimFixed(thousands, thousands < 99.95 ? 1 : 0)}k`;
  return `${trimFixed(tokens / 1_000_000, 1)}M`;
}
