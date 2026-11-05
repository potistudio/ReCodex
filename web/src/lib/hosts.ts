export interface HostSummary {
	id: string;
	name: string;
}
export interface DiscoveredHost extends HostSummary {
	addresses: string[];
}
export interface HostCatalog {
	registered: HostSummary[];
	discovered: DiscoveredHost[];
	discoveryError: string | null;
	hostingError: string | null;
}
export interface HostConnection extends HostSummary {
	connected: boolean;
	connecting: boolean;
	error: string;
}
