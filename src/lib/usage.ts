export function formatTokenCount(tokens: number) {
	return new Intl.NumberFormat().format(tokens);
}
