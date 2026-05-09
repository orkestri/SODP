package sodp

// Collector is the observability hook for the SODP server.
// Implementations may use any backend (Prometheus, StatsD, OpenTelemetry, etc.).
// Pass a nil Collector to WithCollector — the server substitutes a no-op.
type Collector interface {
	SessionOpened()
	SessionClosed()
	MutationApplied(key string)
	DeltaDropped(kind string) // "fanout" | "session" | "pool"
	DeltaDelivered()
}

// noopCollector is the zero-cost default used when no Collector is wired in.
type noopCollector struct{}

func (noopCollector) SessionOpened()         {}
func (noopCollector) SessionClosed()         {}
func (noopCollector) MutationApplied(string) {}
func (noopCollector) DeltaDropped(string)    {}
func (noopCollector) DeltaDelivered()        {}
