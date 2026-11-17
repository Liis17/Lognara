package com.lognara.client.ui.screens.issues

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceApiRoutes
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.GroupRequest
import com.lognara.client.data.GroupResponse
import com.lognara.client.data.Issue
import com.lognara.client.data.IssueDetailsRequest
import com.lognara.client.data.IssueDetailsResponse
import com.lognara.client.data.IssueResponse
import com.lognara.client.data.IssueSearchRequest
import com.lognara.client.data.IssueStatus
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.QueryFilters
import com.lognara.client.data.apiJson
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.TimeWindow
import com.lognara.client.ui.screens.data.timeWindow
import java.util.concurrent.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.encodeToJsonElement

data class IssuesPageData(val response: IssueResponse, val services: List<String>)

data class IssuesUiState(
    val range: DataRange = DataRange.TwentyFourHours,
    val status: IssueStatus = IssueStatus.OPEN,
    val service: String = "",
    val result: DataLoadState<IssuesPageData> = DataLoadState.Loading,
)

fun issueFilters(service: String): QueryFilters =
    if (service.isBlank()) emptyMap() else mapOf("service" to listOf(service.trim()))

fun issueIntervalSeconds(range: DataRange): Long = when (range) {
    DataRange.FifteenMinutes -> 60
    DataRange.OneHour -> 300
    DataRange.TwentyFourHours -> 3_600
    DataRange.SevenDays -> 21_600
}

fun buildIssueSearchRequest(
    window: TimeWindow,
    range: DataRange,
    status: IssueStatus,
    service: String,
): IssueSearchRequest = IssueSearchRequest(
    from = window.from,
    to = window.to,
    filters = issueFilters(service),
    interval_seconds = issueIntervalSeconds(range),
    limit = 100,
    status = status,
)

fun buildIssueServicesRequest(window: TimeWindow): GroupRequest = GroupRequest(
    from = window.from,
    to = window.to,
    filters = mapOf("level" to listOf("error", "fatal")),
    group_by = listOf("service"),
    limit = 100,
)

class IssuesViewModel(private val api: LognaraApi, private val session: DeviceSession) : ViewModel() {
    private val _state = MutableStateFlow(IssuesUiState())
    val state = _state.asStateFlow()
    private var loadJob: Job? = null

    init {
        load()
    }

    fun setRange(range: DataRange) {
        if (_state.value.range == range) return
        _state.value = _state.value.copy(range = range)
        load()
    }

    fun setStatus(status: IssueStatus) {
        if (_state.value.status == status) return
        _state.value = _state.value.copy(status = status)
        load()
    }

    fun setService(service: String) {
        if (_state.value.service == service) return
        _state.value = _state.value.copy(service = service)
        load()
    }

    fun reload() = load()

    private fun load() {
        loadJob?.cancel()
        val current = _state.value
        val window = timeWindow(current.range)
        val request = buildIssueSearchRequest(window, current.range, current.status, current.service)
        val servicesRequest = buildIssueServicesRequest(window)
        _state.value = current.copy(result = DataLoadState.Loading)
        loadJob = viewModelScope.launch {
            try {
                val response = api.authenticatedJson<IssueResponse>(
                    session,
                    "POST",
                    DeviceApiRoutes.ISSUE_SEARCH,
                    apiJson.encodeToJsonElement(IssueSearchRequest.serializer(), request),
                )
                val groups = api.authenticatedJson<GroupResponse>(
                    session,
                    "POST",
                    DeviceApiRoutes.GROUP_BY,
                    apiJson.encodeToJsonElement(GroupRequest.serializer(), servicesRequest),
                )
                _state.value = _state.value.copy(
                    result = DataLoadState.Content(
                        IssuesPageData(
                            response = response,
                            services = groups.groups.mapNotNull { it.values["service"] }.distinct().sorted(),
                        ),
                    ),
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

enum class IssueAction { Resolve, Reopen }

data class IssueDetailsUiState(
    val result: DataLoadState<IssueDetailsResponse> = DataLoadState.Loading,
    val actionPending: Boolean = false,
    val actionError: Boolean = false,
)

class IssueDetailsViewModel(
    private val api: LognaraApi,
    private val session: DeviceSession,
    private val fingerprint: String,
    private val range: DataRange,
    private val service: String,
) : ViewModel() {
    private val _state = MutableStateFlow(IssueDetailsUiState())
    val state = _state.asStateFlow()
    private var loadJob: Job? = null

    init {
        load()
    }

    fun retry() = load()

    fun changeStatus(action: IssueAction, onStatusChanged: () -> Unit) {
        if (_state.value.actionPending) return
        _state.value = _state.value.copy(actionPending = true, actionError = false)
        viewModelScope.launch {
            try {
                val path = when (action) {
                    IssueAction.Resolve -> DeviceApiRoutes.issueResolve(fingerprint)
                    IssueAction.Reopen -> DeviceApiRoutes.issueReopen(fingerprint)
                }
                api.authenticated(session, "POST", path)
                onStatusChanged()
                load()
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _state.value = _state.value.copy(actionError = true)
            } finally {
                _state.value = _state.value.copy(actionPending = false)
            }
        }
    }

    private fun load() {
        loadJob?.cancel()
        val window = timeWindow(range)
        val request = IssueDetailsRequest(
            from = window.from,
            to = window.to,
            filters = issueFilters(service),
            interval_seconds = issueIntervalSeconds(range),
        )
        _state.value = _state.value.copy(result = DataLoadState.Loading)
        loadJob = viewModelScope.launch {
            try {
                val response = api.authenticatedJson<IssueDetailsResponse>(
                    session,
                    "POST",
                    DeviceApiRoutes.issueDetails(fingerprint),
                    apiJson.encodeToJsonElement(IssueDetailsRequest.serializer(), request),
                )
                _state.value = _state.value.copy(result = DataLoadState.Content(response))
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
