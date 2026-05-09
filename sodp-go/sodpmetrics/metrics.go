package sodpmetrics

import (
	"net/http"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// Metrics implements sodp.Collector using Prometheus counters and gauges.
type Metrics struct {
	sessionsActive    prometheus.Gauge
	mutationsTotal    *prometheus.CounterVec
	deltaDropsTotal   *prometheus.CounterVec
	deltaDeliveries   prometheus.Counter
	gatherer          prometheus.Gatherer
}

// New creates a Metrics and registers all instruments with reg.
// Pass nil to use prometheus.DefaultRegisterer / prometheus.DefaultGatherer.
func New(reg prometheus.Registerer) *Metrics {
	var gatherer prometheus.Gatherer
	if reg == nil {
		reg = prometheus.DefaultRegisterer
		gatherer = prometheus.DefaultGatherer
	} else {
		// If the caller passed a *prometheus.Registry it also implements Gatherer.
		if g, ok := reg.(prometheus.Gatherer); ok {
			gatherer = g
		} else {
			gatherer = prometheus.DefaultGatherer
		}
	}

	sessionsActive := prometheus.NewGauge(prometheus.GaugeOpts{
		Name: "sodp_sessions_active",
		Help: "Number of currently active WebSocket sessions.",
	})
	mutationsTotal := prometheus.NewCounterVec(prometheus.CounterOpts{
		Name: "sodp_mutations_total",
		Help: "Total number of state mutations applied.",
	}, []string{"key"})
	deltaDropsTotal := prometheus.NewCounterVec(prometheus.CounterOpts{
		Name: "sodp_delta_drops_total",
		Help: `Total number of deltas dropped by kind ("fanout", "session", "pool").`,
	}, []string{"kind"})
	deltaDeliveries := prometheus.NewCounter(prometheus.CounterOpts{
		Name: "sodp_delta_deliveries_total",
		Help: "Total number of deltas successfully delivered to subscribers.",
	})

	reg.MustRegister(sessionsActive, mutationsTotal, deltaDropsTotal, deltaDeliveries)

	return &Metrics{
		sessionsActive:  sessionsActive,
		mutationsTotal:  mutationsTotal,
		deltaDropsTotal: deltaDropsTotal,
		deltaDeliveries: deltaDeliveries,
		gatherer:        gatherer,
	}
}

// Handler returns an HTTP handler that exposes the Prometheus metrics endpoint.
func (m *Metrics) Handler() http.Handler {
	return promhttp.HandlerFor(m.gatherer, promhttp.HandlerOpts{})
}

func (m *Metrics) SessionOpened() {
	m.sessionsActive.Inc()
}

func (m *Metrics) SessionClosed() {
	m.sessionsActive.Dec()
}

func (m *Metrics) MutationApplied(key string) {
	m.mutationsTotal.WithLabelValues(key).Inc()
}

func (m *Metrics) DeltaDropped(kind string) {
	m.deltaDropsTotal.WithLabelValues(kind).Inc()
}

func (m *Metrics) DeltaDelivered() {
	m.deltaDeliveries.Inc()
}
