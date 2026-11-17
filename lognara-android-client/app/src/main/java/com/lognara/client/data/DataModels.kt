package com.lognara.client.data

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

typealias QueryFilters = Map<String, List<String>>

@Serializable
data class TextQuery(val query: String, val mode: String = "all")

@Serializable
enum class LogLevel {
    @SerialName("trace") TRACE,
    @SerialName("debug") DEBUG,
    @SerialName("info") INFO,
    @SerialName("warn") WARN,
    @SerialName("error") ERROR,
    @SerialName("fatal") FATAL,
    @SerialName("unknown") UNKNOWN,
}

@Serializable
data class LogEvent(
    val id: String,
    val timestamp: String,
    val ingested_at: String,
    val relay_id: Long? = null,
    val agent_id: Long? = null,
    val environment: String? = null,
    val server: String,
    val backend: String,
    val service: String,
    val service_instance: String? = null,
    val action: String? = null,
    val level: LogLevel,
    val message: String,
    val trace_id: String? = null,
    val span_id: String? = null,
    val parent_span_id: String? = null,
    val request_id: String? = null,
    val attributes: Map<String, JsonElement> = emptyMap(),
)

@Serializable
data class SearchRequest(
    val from: String,
    val to: String,
    val filters: QueryFilters = emptyMap(),
    val text: TextQuery? = null,
    val limit: Int = 50,
    val cursor: String? = null,
)

@Serializable
data class SearchResponse(val events: List<LogEvent>, val next_cursor: String? = null, val as_of: String)

@Serializable
data class SummaryRequest(
    val from: String,
    val to: String,
    val filters: QueryFilters = emptyMap(),
    val text: TextQuery? = null,
    val interval_seconds: Long = 60,
    val search_limit: Int = 50,
)

@Serializable
data class LevelCounts(
    val trace: Long,
    val debug: Long,
    val info: Long,
    val warn: Long,
    val error: Long,
    val fatal: Long,
    val unknown: Long,
)

@Serializable
data class SummaryBucket(val timestamp: String, val count: Long, val levels: LevelCounts)

@Serializable
data class HistogramBucket(val timestamp: String, val count: Long, val levels: LevelCounts? = null)

@Serializable
data class SummaryResponse(
    val as_of: String,
    val total: Long,
    val buckets: List<SummaryBucket>,
    val levels: LevelCounts,
    val error_services: List<SummaryErrorService>,
    val duration: DurationStats,
    val actions: List<SummaryAction>,
    val page: SearchResponse? = null,
)

@Serializable
data class SummaryErrorService(val service: String? = null, val count: Long)

@Serializable
data class DurationStats(
    val p95_ms: Double? = null,
    val measured: Long,
    val coverage: Double,
    val method: String,
)

@Serializable
data class SummaryAction(val action: String? = null, val count: Long, val duration: DurationStats)

@Serializable
data class GroupRequest(
    val from: String,
    val to: String,
    val filters: QueryFilters = emptyMap(),
    val group_by: List<String>,
    val limit: Int = 100,
)

@Serializable
data class GroupResponse(val groups: List<LogGroup>, val as_of: String)

@Serializable
data class LogGroup(val values: Map<String, String?>, val count: Long)

@Serializable
enum class IssueStatus {
    @SerialName("open") OPEN,
    @SerialName("new") NEW,
    @SerialName("regressed") REGRESSED,
    @SerialName("resolved") RESOLVED,
}

@Serializable
data class IssueSearchRequest(
    val from: String,
    val to: String,
    val filters: QueryFilters = emptyMap(),
    val interval_seconds: Long = 300,
    val limit: Int = 100,
    val status: IssueStatus = IssueStatus.OPEN,
)

@Serializable
data class IssueResponse(
    val algorithm_version: Int,
    val as_of: String,
    val watermark: Long,
    val core_watermark: Long,
    val history_from: String? = null,
    val history_complete: Boolean,
    val counts: IssueCounts,
    val status_events: Long,
    val issues: List<Issue>,
)

@Serializable
data class IssueCounts(val open: Long, val new: Long? = null, val regressed: Long, val resolved: Long)

@Serializable
data class Issue(
    val fingerprint: String,
    val environment: String? = null,
    val service: String,
    val action: String? = null,
    val exception_type: String? = null,
    val frames: List<String> = emptyList(),
    val pattern_id: String,
    val template: String,
    val parts: List<JsonElement> = emptyList(),
    val exact: Boolean,
    val count: Long,
    val servers: Long,
    val is_new: Boolean? = null,
    val first_seen: String,
    val last_seen: String,
    val buckets: List<HistogramBucket> = emptyList(),
    val state: IssueState? = null,
)

@Serializable
data class IssueState(val status: String, val resolved_at: String? = null, val resolved_by: String? = null)

@Serializable
data class IssueDetailsRequest(
    val from: String,
    val to: String,
    val filters: QueryFilters = emptyMap(),
    val interval_seconds: Long = 300,
)

@Serializable
data class IssueDetailsResponse(
    val as_of: String,
    val issue: Issue,
    val servers: List<IssueServer>,
    val other_servers: Long,
    val examples: List<LogEvent>,
    val state: IssueState,
    val history: List<IssueHistory>,
)

@Serializable
data class IssueServer(val value: String, val count: Long)

@Serializable
data class IssueHistory(
    val action: String,
    val actor: String? = null,
    val at: String,
    val event_at: String? = null,
)

@Serializable
data class InfrastructureResponse(
    val as_of: String,
    val thresholds: InfrastructureThresholds,
    val core: CoreStatus,
    val storage: InfrastructureStorage? = null,
    val dock: DockStatus,
    val registry: Boolean,
    val nodes: List<InfrastructureNode>,
)

@Serializable
data class InfrastructureThresholds(val heartbeat_ttl_seconds: Long, val idle_seconds: Long, val spool_warn_percent: Long)

@Serializable
data class CoreStatus(
    val status: String,
    val version: String? = null,
    val release_version: String? = null,
    val git_commit: String? = null,
    val visibility_lag_ms: Long? = null,
    val wal_bytes: Long? = null,
    val wal_limit_bytes: Long? = null,
)

@Serializable
data class InfrastructureStorage(
    val segments: Long,
    val parquet_bytes: Long,
    val index_bytes: Long,
    val oldest_event_at: String? = null,
    val retention_seconds: Long,
    val disk_total_bytes: Long,
    val disk_free_bytes: Long,
)

@Serializable
data class DockStatus(
    val version: String,
    val release_version: String? = null,
    val git_commit: String? = null,
    val sse_clients: Long,
    val sessions: Long? = null,
)

@Serializable
data class InfrastructureNode(
    val kind: String,
    val id: Long,
    val name: String,
    val relay_id: Long? = null,
    val agent_id: Long? = null,
    val relay: String? = null,
    val status: String,
    val reason: String? = null,
    val heartbeat_at: String? = null,
    val last_event_at: String? = null,
    val version: String? = null,
    val release_version: String? = null,
    val spool: SpoolUsage? = null,
    val delivery_failures: Long? = null,
    val events_per_second: Double? = null,
    val source: InfrastructureSource? = null,
)

@Serializable
data class SpoolUsage(val used_bytes: Long, val limit_bytes: Long, val records: Long)

@Serializable
data class InfrastructureSource(
    val environment: String? = null,
    val server: String,
    val backend: String,
    val service: String,
    val service_instance: String? = null,
)

@Serializable
data class MetricKey(val server: String, val service: String, val name: String, val origin: String)

@Serializable
data class MetricSource(val server: String, val service: String)

@Serializable
data class MetricSourcesResponse(val sources: List<MetricSource>)

@Serializable
data class SeriesMetric(
    val server: String,
    val service: String,
    val name: String,
    val origin: String,
    val first_seen: String? = null,
    val last_seen: String? = null,
    val definition: SeriesDefinition? = null,
    val environments: List<String> = emptyList(),
)

@Serializable
data class SeriesDefinition(
    val server: String,
    val id: String,
    val service: String,
    val name: String,
    val title: String,
    val description: String,
    val unit: String,
    val kind: String,
    val aggregation: String,
    val enabled: Boolean,
    val rule: JsonElement? = null,
    val updated_at: String,
)

@Serializable
data class SeriesMetricsResponse(val metrics: List<SeriesMetric>)

@Serializable
data class MetricFavoritesResponse(val keys: List<MetricKey>)

@Serializable
data class MetricLookupRequest(val keys: List<MetricKey>)

@Serializable
data class SeriesQuery(
    val server: String? = null,
    val origin: String? = null,
    val service: String,
    val name: String,
    val from: String,
    val to: String,
    val step_seconds: Long,
    val split: String,
    val environment: String? = null,
)

@Serializable
data class SeriesReply(
    val kind: String,
    val aggregation: String,
    val unit: String,
    val buckets: List<String>,
    val series: List<SeriesLine>,
)

@Serializable
data class SeriesLine(val label: String, val values: List<Double?>)
