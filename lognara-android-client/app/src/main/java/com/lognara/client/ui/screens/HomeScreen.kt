package com.lognara.client.ui.screens

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import com.lognara.client.R
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceApiRoutes
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.Issue
import com.lognara.client.data.IssueResponse
import com.lognara.client.data.IssueSearchRequest
import com.lognara.client.data.IssueStatus
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SeriesMetric
import com.lognara.client.data.apiJson
import com.lognara.client.ui.components.BottomNavItem
import com.lognara.client.ui.components.LognaraBottomBar
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.timeWindow
import com.lognara.client.ui.screens.infra.InfrastructureScreen
import com.lognara.client.ui.screens.issues.IssuesScreen
import com.lognara.client.ui.screens.issues.buildIssueSearchRequest
import com.lognara.client.ui.screens.live.LiveScreen
import com.lognara.client.ui.screens.logs.LogsScreen
import com.lognara.client.ui.screens.metrics.MetricsScreen
import java.util.concurrent.CancellationException
import kotlinx.serialization.json.encodeToJsonElement

@Composable
fun HomeScreen(
    session: DeviceSession,
    api: LognaraApi,
    onSettings: () -> Unit,
    onOpenEvent: (LogEvent) -> Unit,
    onOpenIssue: (Issue, DataRange, String) -> Unit,
    issuesRefreshKey: Int,
    onIssueStatusChanged: () -> Unit,
    onOpenMetric: (SeriesMetric) -> Unit,
) {
    var selectedTab by rememberSaveable { mutableIntStateOf(0) }
    var openIssues by rememberSaveable { mutableStateOf<Long?>(null) }

    // Бейдж «Проблемы»: число открытых проблем за 24 ч, обновляется после resolve/reopen.
    LaunchedEffect(session.deviceId, issuesRefreshKey) {
        openIssues = try {
            val request = buildIssueSearchRequest(
                timeWindow(DataRange.TwentyFourHours),
                DataRange.TwentyFourHours,
                IssueStatus.OPEN,
                "",
            ).copy(limit = 1)
            api.authenticatedJson<IssueResponse>(
                session,
                "POST",
                DeviceApiRoutes.ISSUE_SEARCH,
                apiJson.encodeToJsonElement(IssueSearchRequest.serializer(), request),
            ).counts.open
        } catch (exception: CancellationException) {
            throw exception
        } catch (_: ApiException.Unauthorized) {
            null
        } catch (_: Exception) {
            openIssues
        }
    }

    val tabs = listOf(
        BottomNavItem(R.drawable.ic_nav_logs, stringResource(R.string.tab_logs)),
        BottomNavItem(R.drawable.ic_nav_live, stringResource(R.string.tab_live)),
        BottomNavItem(R.drawable.ic_nav_issues, stringResource(R.string.tab_issues), badge = openIssues),
        BottomNavItem(R.drawable.ic_nav_infra, stringResource(R.string.tab_infrastructure)),
        BottomNavItem(R.drawable.ic_nav_metrics, stringResource(R.string.tab_metrics)),
    )

    Column(Modifier.fillMaxSize()) {
        Box(Modifier.weight(1f).statusBarsPadding()) {
            when (selectedTab) {
                0 -> LogsScreen(api, session, onOpenEvent, onSettings)
                1 -> LiveScreen(api, session, onOpenEvent, onSettings)
                2 -> IssuesScreen(api, session, issuesRefreshKey, onOpenIssue, onIssueStatusChanged, onSettings)
                3 -> InfrastructureScreen(api, session, onSettings)
                else -> MetricsScreen(api, session, onOpenMetric, onSettings)
            }
        }
        LognaraBottomBar(tabs, selectedTab) { selectedTab = it }
    }
}
