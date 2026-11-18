package com.lognara.client.ui.screens.logs

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceApiRoutes
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LogLevel
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SearchRequest
import com.lognara.client.data.SearchResponse
import com.lognara.client.data.SummaryRequest
import com.lognara.client.data.SummaryResponse
import com.lognara.client.data.TextQuery
import com.lognara.client.data.apiJson
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.TimeWindow
import com.lognara.client.ui.screens.data.intervalSeconds
import com.lognara.client.ui.screens.data.timeWindow
import java.util.concurrent.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.encodeToJsonElement

data class LogPageData(
    val summary: SummaryResponse?,
    val events: List<LogEvent>,
    val nextCursor: String?,
    val summaryRequest: SummaryRequest? = null,
)

data class LogsUiState(
    val range: DataRange = DataRange.OneHour,
    val service: String = "",
    val query: String = "",
    val levels: Set<LogLevel> = LogLevel.entries.toSet(),
    val result: DataLoadState<LogPageData> = DataLoadState.Loading,
    val loadingMore: Boolean = false,
    val pagingError: Boolean = false,
)

fun buildLogFilters(levels: Set<LogLevel>, service: String): Map<String, List<String>> = buildMap {
    if (levels.size != LogLevel.entries.size) put("level", levels.map(::levelValue))
    if (service.isNotBlank()) put("service", listOf(service.trim()))
}

fun buildSummaryRequest(
    window: TimeWindow,
    range: DataRange,
    query: String,
    service: String,
    levels: Set<LogLevel>,
): SummaryRequest = SummaryRequest(
    from = window.from,
    to = window.to,
    filters = buildLogFilters(levels, service),
    text = query.trim().takeIf(String::isNotEmpty)?.let(::TextQuery),
    interval_seconds = intervalSeconds(range),
    search_limit = 50,
)

fun buildNextPageRequest(summaryRequest: SummaryRequest, cursor: String): SearchRequest = SearchRequest(
    from = summaryRequest.from,
    to = summaryRequest.to,
    filters = summaryRequest.filters,
    text = summaryRequest.text,
    limit = summaryRequest.search_limit,
    cursor = cursor,
)

private fun levelValue(level: LogLevel): String = when (level) {
    LogLevel.TRACE -> "trace"
    LogLevel.DEBUG -> "debug"
    LogLevel.INFO -> "info"
    LogLevel.WARN -> "warn"
    LogLevel.ERROR -> "error"
    LogLevel.FATAL -> "fatal"
    LogLevel.UNKNOWN -> "unknown"
}

class LogsViewModel(private val api: LognaraApi, private val session: DeviceSession) : ViewModel() {
    private val _state = MutableStateFlow(LogsUiState())
    val state = _state.asStateFlow()
    private var loadJob: Job? = null
    private var pagingJob: Job? = null

    init {
        load()
    }

    fun setRange(range: DataRange) {
        if (_state.value.range == range) return
        _state.value = _state.value.copy(range = range)
        load()
    }

    fun setLevels(levels: Set<LogLevel>) {
        if (_state.value.levels == levels) return
        _state.value = _state.value.copy(levels = levels)
        load()
    }

    fun search(query: String, service: String) {
        _state.value = _state.value.copy(query = query, service = service)
        load()
    }

    fun reload() = load()

    fun loadNextPage() {
        val current = _state.value
        val data = (current.result as? DataLoadState.Content)?.value ?: return
        val cursor = data.nextCursor ?: return
        if (current.loadingMore || current.levels.isEmpty()) return
        val summaryRequest = data.summaryRequest ?: return
        pagingJob?.cancel()
        _state.value = current.copy(loadingMore = true, pagingError = false)
        pagingJob = viewModelScope.launch {
            try {
                val request = buildNextPageRequest(summaryRequest, cursor)
                val next = api.authenticatedJson<SearchResponse>(
                    session,
                    "POST",
                    DeviceApiRoutes.LOG_SEARCH,
                    apiJson.encodeToJsonElement(SearchRequest.serializer(), request),
                )
                val latest = (_state.value.result as? DataLoadState.Content)?.value ?: return@launch
                val seen = latest.events.mapTo(HashSet()) { it.id }
                val appended = latest.events + next.events.filter { seen.add(it.id) }
                _state.value = _state.value.copy(
                    result = DataLoadState.Content(latest.copy(events = appended, nextCursor = next.next_cursor)),
                    pagingError = false,
                )
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _state.value = _state.value.copy(pagingError = true)
            } finally {
                _state.value = _state.value.copy(loadingMore = false)
            }
        }
    }

    private fun load() {
        loadJob?.cancel()
        pagingJob?.cancel()
        if (_state.value.levels.isEmpty()) {
            _state.value = _state.value.copy(
                result = DataLoadState.Content(LogPageData(null, emptyList(), null)),
                loadingMore = false,
                pagingError = false,
            )
            return
        }
        val range = _state.value.range
        val window = timeWindow(range)
        val request = buildSummaryRequest(window, range, _state.value.query, _state.value.service, _state.value.levels)
        _state.value = _state.value.copy(result = DataLoadState.Loading, loadingMore = false, pagingError = false)
        loadJob = viewModelScope.launch {
            try {
                val summary = api.authenticatedJson<SummaryResponse>(
                    session,
                    "POST",
                    DeviceApiRoutes.SUMMARY,
                    apiJson.encodeToJsonElement(SummaryRequest.serializer(), request),
                )
                val page = summary.page ?: SearchResponse(emptyList(), null, summary.as_of)
                _state.value = _state.value.copy(
                    result = DataLoadState.Content(LogPageData(summary, page.events, page.next_cursor, request)),
                )
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _state.value = _state.value.copy(result = DataLoadState.Error)
            }
        }
    }
}
