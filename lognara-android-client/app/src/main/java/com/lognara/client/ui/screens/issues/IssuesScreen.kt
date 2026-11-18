package com.lognara.client.ui.screens.issues

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.HistogramBucket
import com.lognara.client.data.Issue
import com.lognara.client.data.IssueDetailsResponse
import com.lognara.client.data.IssueStatus
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LognaraApi
import com.lognara.client.ui.components.CardShape
import com.lognara.client.ui.components.ChipRow
import com.lognara.client.ui.components.DetailTopBar
import com.lognara.client.ui.components.FilterPill
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.PillButton
import com.lognara.client.ui.components.RowDivider
import com.lognara.client.ui.components.ScreenHeader
import com.lognara.client.ui.components.SectionLabel
import com.lognara.client.ui.components.SettingsButton
import com.lognara.client.ui.components.StatTile
import com.lognara.client.ui.components.StatusPill
import com.lognara.client.ui.components.TagPill
import com.lognara.client.ui.components.UsageBar
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.LognaraPalette
import com.lognara.client.ui.theme.LognaraSemanticColors
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun IssuesScreen(
    api: LognaraApi,
    session: DeviceSession,
    refreshKey: Int,
    onOpenIssue: (Issue, DataRange, String) -> Unit,
    onStatusChanged: () -> Unit,
    onSettings: () -> Unit,
) {
    val viewModel: IssuesViewModel = viewModel(key = "issues-${session.deviceId}") {
        IssuesViewModel(api, session)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val content = (state.result as? DataLoadState.Content)?.value
    var sortName by rememberSaveable { mutableStateOf(IssueSort.FREQUENT.name) }
    val sort = IssueSort.entries.firstOrNull { it.name == sortName } ?: IssueSort.FREQUENT

    LaunchedEffect(refreshKey) {
        if (refreshKey > 0) viewModel.reload()
    }

    Column(Modifier.fillMaxSize()) {
        ScreenHeader(
            title = stringResource(R.string.tab_issues),
            subtitle = stringResource(R.string.issue_subtitle, stringResource(state.range.label)),
            actions = { SettingsButton(onSettings) },
        )
        ChipRow {
            IssueStatus.entries.forEach { status ->
                val selected = state.status == status
                FilterPill(
                    text = stringResource(statusLabel(status)),
                    selected = selected,
                    onClick = { viewModel.setStatus(status) },
                    trailing = {
                        Text(
                            statusCount(status, content),
                            style = monoStyle(12, FontWeight.W500),
                            color = if (selected) LognaraPalette.OnAccent else LognaraPalette.TextMuted,
                        )
                    },
                )
            }
        }
        com.lognara.client.ui.screens.data.RangeSelector(
            selected = state.range,
            onSelect = viewModel::setRange,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            IssueServiceSelector(
                selected = state.service,
                services = content?.services.orEmpty(),
                onSelect = viewModel::setService,
            )
            IssueSortSelector(sort = sort, onSelect = { sortName = it.name })
        }
        PullToRefreshBox(
            isRefreshing = state.result is DataLoadState.Loading,
            onRefresh = viewModel::reload,
            modifier = Modifier.weight(1f).fillMaxWidth(),
        ) {
            DataStateContent(
                state = state.result,
                isEmpty = { false },
                onRetry = viewModel::reload,
                modifier = Modifier.fillMaxSize(),
            ) { data ->
                if (data.response.issues.isEmpty()) {
                    Box(Modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) {
                        Text(
                            text = if (state.status == IssueStatus.NEW && data.response.counts.new == null) {
                                stringResource(R.string.issue_new_unknown)
                            } else {
                                stringResource(R.string.issue_empty)
                            },
                            color = LognaraPalette.TextMuted,
                        )
                    }
                } else {
                    val sortedIssues = remember(data.response.issues, sort, state.status) {
                        sortIssues(data.response.issues, sort, state.status)
                    }
                    LazyColumn(
                        modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                        contentPadding = PaddingValues(top = 12.dp, bottom = 16.dp),
                    ) {
                        items(sortedIssues, key = { it.fingerprint }) { issue ->
                            IssueCard(issue) { onOpenIssue(issue, state.range, state.service) }
                        }
                    }
                }
            }
        }
    }
}

private fun statusLabel(status: IssueStatus): Int = when (status) {
    IssueStatus.OPEN -> R.string.issue_filter_open
    IssueStatus.NEW -> R.string.issue_filter_new
    IssueStatus.REGRESSED -> R.string.issue_filter_regressed
    IssueStatus.RESOLVED -> R.string.issue_filter_resolved
}

private fun statusCount(status: IssueStatus, data: IssuesPageData?): String = when (status) {
    IssueStatus.OPEN -> data?.response?.counts?.open?.toString() ?: "0"
    IssueStatus.NEW -> data?.response?.counts?.new?.toString() ?: "—"
    IssueStatus.REGRESSED -> data?.response?.counts?.regressed?.toString() ?: "0"
    IssueStatus.RESOLVED -> data?.response?.counts?.resolved?.toString() ?: "0"
}

internal enum class IssueSort { FREQUENT, NEWEST }

internal fun sortIssues(issues: List<Issue>, sort: IssueSort, status: IssueStatus? = null): List<Issue> {
    if (status == IssueStatus.RESOLVED) return issues
    return when (sort) {
        IssueSort.FREQUENT -> issues.sortedWith(
            compareByDescending<Issue> { it.count }
                .thenByDescending { issueInstant(it.last_seen) },
        )
        IssueSort.NEWEST -> issues.sortedWith(
            compareByDescending<Issue> { issueInstant(it.last_seen) }
                .thenByDescending { it.count },
        )
    }
}

private fun issueInstant(timestamp: String): Instant =
    runCatching { Instant.parse(timestamp) }.getOrDefault(Instant.MIN)

@Composable
private fun IssueServiceSelector(selected: String, services: List<String>, onSelect: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        FilterPill(
            text = selected.ifBlank { stringResource(R.string.issue_service_all) },
            onClick = { expanded = true },
            leading = {
                Icon(
                    painterResource(R.drawable.ic_services),
                    contentDescription = null,
                    tint = LognaraPalette.TextSecondary,
                    modifier = Modifier.size(18.dp),
                )
            },
            trailing = { ChevronDown() },
        )
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text(stringResource(R.string.issue_service_all)) },
                onClick = { onSelect(""); expanded = false },
            )
            services.forEach { service ->
                DropdownMenuItem(
                    text = { Text(service) },
                    onClick = { onSelect(service); expanded = false },
                )
            }
        }
    }
}

@Composable
private fun IssueSortSelector(sort: IssueSort, onSelect: (IssueSort) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        FilterPill(
            text = stringResource(if (sort == IssueSort.FREQUENT) R.string.issue_sort_frequent else R.string.issue_sort_newest),
            onClick = { expanded = true },
            trailing = { ChevronDown() },
        )
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            IssueSort.entries.forEach { option ->
                DropdownMenuItem(
                    text = {
                        Text(stringResource(if (option == IssueSort.FREQUENT) R.string.issue_sort_frequent else R.string.issue_sort_newest))
                    },
                    onClick = { onSelect(option); expanded = false },
                )
            }
        }
    }
}

@Composable
private fun ChevronDown() {
    Icon(
        painterResource(R.drawable.ic_chevron_down),
        contentDescription = null,
        tint = LognaraPalette.TextMuted,
        modifier = Modifier.size(16.dp),
    )
}

@Composable
private fun IssueCard(issue: Issue, onClick: () -> Unit) {
    val statusIssue = issue
    val statusColor = issueStatusColor(statusIssue)
    LognaraCard(
        modifier = Modifier.fillMaxWidth(),
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
        shape = RoundedCornerShape(14.dp),
        onClick = onClick,
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            StatusPill(issueStatusLabel(statusIssue), statusColor)
            Text(
                listOfNotNull(issue.service, issue.action, issue.environment).joinToString(" · "),
                color = LognaraPalette.TextMuted,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Icon(
                painterResource(R.drawable.ic_chevron_right),
                contentDescription = null,
                tint = LognaraPalette.TextMuted,
                modifier = Modifier.size(18.dp),
            )
        }
        IssueMessageText(
            text = issue.exception_type ?: issue.template,
            modifier = Modifier.fillMaxWidth(),
            maxLines = 3,
            styleSize = 13,
        )
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    issue.count.toString(),
                    fontSize = 22.sp,
                    lineHeight = 27.sp,
                    fontWeight = FontWeight.W600,
                    color = LognaraPalette.Text,
                )
                Text(issueCountSummary(issue), fontSize = 12.sp, color = LognaraPalette.TextMuted, maxLines = 1)
            }
            IssueSparkline(
                buckets = issue.buckets,
                color = statusColor,
                modifier = Modifier.weight(1f),
                height = 28.dp,
            )
        }
        val lastEventLabel = stringResource(R.string.issue_last_event_label)
        Text(
            text = buildAnnotatedString {
                withStyle(SpanStyle(color = LognaraPalette.TextMuted)) {
                    append(lastEventLabel)
                    append(' ')
                }
                withStyle(SpanStyle(fontFamily = monoStyle(12).fontFamily, color = LognaraPalette.TextSecondary)) {
                    append(localTimestamp(issue.last_seen))
                }
            },
            fontSize = 12.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun issueCountSummary(issue: Issue): String {
    val resources = LocalContext.current.resources
    val eventQuantity = issue.count.coerceIn(0, Int.MAX_VALUE.toLong()).toInt()
    val serverQuantity = issue.servers.coerceIn(0, Int.MAX_VALUE.toLong()).toInt()
    val eventText = resources.getQuantityString(R.plurals.issue_events_count, eventQuantity, issue.count)
    val serverText = resources.getQuantityString(R.plurals.issue_servers_count, serverQuantity, issue.servers)
    return stringResource(R.string.issue_count_summary, eventText, serverText)
}

@Composable
private fun IssueMessageText(
    text: String,
    modifier: Modifier = Modifier,
    maxLines: Int = 2,
    styleSize: Int = 15,
) {
    val regex = remember { Regex("<[^>]+>") }
    val message = remember(text) {
        buildAnnotatedString {
            var currentIndex = 0
            regex.findAll(text).forEach { match ->
                append(text.substring(currentIndex, match.range.first))
                withStyle(SpanStyle(color = LognaraPalette.Accent, background = LognaraPalette.Accent.copy(alpha = 0.14f))) {
                    append(match.value)
                }
                currentIndex = match.range.last + 1
            }
            append(text.substring(currentIndex))
        }
    }
    Text(
        text = message,
        modifier = modifier,
        style = monoStyle(styleSize, FontWeight.W500),
        color = LognaraPalette.Text,
        maxLines = maxLines,
        overflow = TextOverflow.Ellipsis,
    )
}

@Composable
private fun issueStatusLabel(issue: Issue): String = when {
    issue.state?.status == "resolved" -> stringResource(R.string.issue_state_resolved)
    issue.state?.status == "regressed" -> stringResource(R.string.issue_state_regressed)
    issue.is_new == true -> stringResource(R.string.issue_state_new)
    else -> stringResource(R.string.issue_state_open)
}

@Composable
private fun issueStatusColor(issue: Issue): Color = when {
    issue.state?.status == "resolved" -> LognaraSemanticColors.statusOk
    issue.state?.status == "regressed" -> LognaraSemanticColors.statusWarn
    issue.is_new == true -> LognaraSemanticColors.statusInfo
    else -> LognaraSemanticColors.statusErr
}

@Composable
fun IssueDetailsScreen(
    api: LognaraApi,
    session: DeviceSession,
    fingerprint: String,
    range: DataRange,
    service: String,
    onBack: () -> Unit,
    onStatusChanged: () -> Unit,
    onOpenEvent: (LogEvent) -> Unit,
) {
    val viewModel: IssueDetailsViewModel = viewModel(key = "issue-${session.deviceId}-$fingerprint-${range.name}-$service") {
        IssueDetailsViewModel(api, session, fingerprint, range, service)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    var confirmAction by rememberSaveable { mutableStateOf<IssueAction?>(null) }
    val details = (state.result as? DataLoadState.Content)?.value

    Column(
        Modifier.fillMaxSize().background(LognaraPalette.Background).statusBarsPadding(),
    ) {
        DetailTopBar(title = stringResource(R.string.issue_detail_title), onBack = onBack)
        if (details == null) {
            DataStateContent(
                state = state.result,
                isEmpty = { false },
                onRetry = viewModel::retry,
                modifier = Modifier.weight(1f).fillMaxWidth().padding(horizontal = 16.dp),
            ) { }
        } else {
            IssueDetailsContent(
                details = details,
                range = range,
                onOpenEvent = onOpenEvent,
                modifier = Modifier.weight(1f),
            )
            Column(Modifier.fillMaxWidth().background(LognaraPalette.Surface).navigationBarsPadding()) {
                RowDivider()
                if (state.actionError) {
                    Text(
                        stringResource(R.string.issue_action_error),
                        color = LognaraSemanticColors.statusErr,
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    PillButton(
                        text = stringResource(
                            if (state.actionPending) R.string.issue_action_loading
                            else if (details.state.status == "resolved") R.string.issue_reopen
                            else R.string.issue_resolve,
                        ),
                        onClick = {
                            confirmAction = if (details.state.status == "resolved") IssueAction.Reopen else IssueAction.Resolve
                        },
                        modifier = Modifier.fillMaxWidth(),
                        primary = true,
                        enabled = !state.actionPending,
                    )
                }
            }
        }
    }

    confirmAction?.let { action ->
        val resolve = action == IssueAction.Resolve
        AlertDialog(
            onDismissRequest = { if (!state.actionPending) confirmAction = null },
            title = {
                Text(stringResource(if (resolve) R.string.issue_confirm_resolve_title else R.string.issue_confirm_reopen_title))
            },
            text = {
                Text(stringResource(if (resolve) R.string.issue_confirm_resolve_message else R.string.issue_confirm_reopen_message))
            },
            confirmButton = {
                TextButton(
                    enabled = !state.actionPending,
                    onClick = {
                        confirmAction = null
                        viewModel.changeStatus(action, onStatusChanged)
                    },
                ) {
                    Text(stringResource(if (resolve) R.string.issue_resolve else R.string.issue_reopen))
                }
            },
            dismissButton = {
                TextButton(enabled = !state.actionPending, onClick = { confirmAction = null }) {
                    Text(stringResource(R.string.cancel))
                }
            },
        )
    }
}

@Composable
private fun IssueDetailsContent(
    details: IssueDetailsResponse,
    range: DataRange,
    onOpenEvent: (LogEvent) -> Unit,
    modifier: Modifier = Modifier,
) {
    val statusIssue = details.issue.copy(state = details.state)
    val statusColor = issueStatusColor(statusIssue)
    LazyColumn(
        modifier = modifier.fillMaxWidth().padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        contentPadding = PaddingValues(top = 24.dp, bottom = 24.dp),
    ) {
        item {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(horizontal = 4.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                    StatusPill(issueStatusLabel(statusIssue), statusColor)
                    TagPill(details.issue.service)
                    details.issue.environment?.let { TagPill(it, mono = true) }
                }
                IssueMessageText(
                    text = details.issue.exception_type ?: details.issue.template,
                    modifier = Modifier.fillMaxWidth(),
                    maxLines = 3,
                    styleSize = 16,
                )
            }
        }
        item {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                StatTile(
                    stringResource(R.string.issue_stat_events),
                    details.issue.count.toString(),
                    modifier = Modifier.weight(1f),
                )
                StatTile(
                    stringResource(R.string.issue_stat_servers),
                    details.issue.servers.toString(),
                    modifier = Modifier.weight(1f),
                )
            }
        }
        item {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                StatTile(
                    stringResource(R.string.issue_stat_first),
                    issueTileTimestamp(details.issue.first_seen),
                    modifier = Modifier.weight(1f),
                    mono = true,
                )
                StatTile(
                    stringResource(R.string.issue_stat_last),
                    issueTileTimestamp(details.issue.last_seen),
                    modifier = Modifier.weight(1f),
                    mono = true,
                )
            }
        }
        item {
            LognaraCard(
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
                shape = CardShape,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(stringResource(R.string.issue_chart_title), fontSize = 15.sp, fontWeight = FontWeight.W600)
                    Text(stringResource(range.label), fontSize = 12.sp, color = LognaraPalette.TextMuted)
                }
                IssueSparkline(details.issue.buckets, statusColor, height = 48.dp)
            }
        }
        item { FingerprintCard(details.issue.fingerprint) }
        item {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(stringResource(R.string.issue_examples), fontSize = 16.sp, fontWeight = FontWeight.W600)
                Text(
                    stringResource(R.string.issue_examples_count, details.examples.size),
                    fontSize = 12.sp,
                    color = LognaraPalette.TextMuted,
                )
            }
        }
        if (details.examples.isEmpty()) {
            item {
                Text(stringResource(R.string.issue_no_examples), color = LognaraPalette.TextMuted)
            }
        } else {
            item {
                LognaraCard(
                    contentPadding = PaddingValues(0.dp),
                    shape = CardShape,
                    verticalArrangement = Arrangement.spacedBy(0.dp),
                ) {
                    details.examples.forEachIndexed { index, event ->
                        IssueExampleRow(event, statusColor) { onOpenEvent(event) }
                        if (index < details.examples.lastIndex) RowDivider()
                    }
                }
            }
        }
        item {
            Text(
                stringResource(R.string.issue_servers),
                fontSize = 16.sp,
                fontWeight = FontWeight.W600,
                modifier = Modifier.padding(horizontal = 4.dp),
            )
        }
        if (details.servers.isEmpty()) {
            item { Text(stringResource(R.string.issue_no_servers), color = LognaraPalette.TextMuted) }
        } else {
            item {
                val maxCount = details.servers.maxOf { it.count }.coerceAtLeast(1)
                LognaraCard(
                    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp),
                    shape = RoundedCornerShape(14.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    details.servers.forEach { server ->
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                                Text(server.value, style = monoStyle(12), color = LognaraPalette.TextSecondary)
                                Text(server.count.toString(), style = monoStyle(12), color = LognaraPalette.Text)
                            }
                            UsageBar(
                                fraction = server.count.toFloat() / maxCount.toFloat(),
                                color = statusColor,
                            )
                        }
                    }
                }
            }
        }
        if (details.other_servers > 0) {
            item {
                Text(
                    stringResource(R.string.issue_other_servers, details.other_servers),
                    modifier = Modifier.padding(horizontal = 8.dp),
                    fontSize = 12.sp,
                    color = LognaraPalette.TextMuted,
                )
            }
        }
        item {
            SectionLabel(stringResource(R.string.issue_history), Modifier.padding(horizontal = 4.dp))
        }
        if (details.history.isEmpty()) {
            item {
                LognaraCard(contentPadding = PaddingValues(14.dp), shape = RoundedCornerShape(14.dp)) {
                    Text(stringResource(R.string.issue_no_history), fontSize = 13.sp, color = LognaraPalette.TextMuted)
                }
            }
        } else {
            item {
                LognaraCard(
                    contentPadding = PaddingValues(0.dp),
                    shape = CardShape,
                    verticalArrangement = Arrangement.spacedBy(0.dp),
                ) {
                    details.history.forEachIndexed { index, history ->
                        Text(
                            stringResource(
                                when (history.action) {
                                    "resolved" -> R.string.issue_history_resolved
                                    "reopened" -> R.string.issue_history_reopened
                                    else -> R.string.issue_history_regressed
                                },
                                localTimestamp(history.at),
                                history.actor ?: stringResource(R.string.issue_actor_system),
                            ),
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
                            fontSize = 13.sp,
                            lineHeight = 19.sp,
                            color = LognaraPalette.TextSecondary,
                        )
                        if (index < details.history.lastIndex) RowDivider()
                    }
                }
            }
        }
    }
}

@Composable
private fun FingerprintCard(fingerprint: String) {
    val clipboard = LocalContext.current.getSystemService(ClipboardManager::class.java)
    LognaraCard(
        contentPadding = PaddingValues(horizontal = 14.dp, vertical = 0.dp),
        shape = RoundedCornerShape(12.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.heightIn(min = 48.dp)) {
            SectionLabel(stringResource(R.string.issue_fingerprint_label))
            Text(
                fingerprint,
                modifier = Modifier.weight(1f).padding(start = 10.dp),
                style = monoStyle(12),
                color = LognaraPalette.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            IconButton(onClick = { clipboard?.setPrimaryClip(ClipData.newPlainText("fingerprint", fingerprint)) }) {
                Icon(
                    painterResource(R.drawable.ic_copy),
                    contentDescription = stringResource(R.string.issue_copy_fingerprint),
                    tint = LognaraPalette.TextMuted,
                    modifier = Modifier.size(18.dp),
                )
            }
        }
    }
}

@Composable
private fun IssueExampleRow(event: LogEvent, color: Color, onClick: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Box(Modifier.size(8.dp).clip(CircleShape).background(color))
            Text(
                localTimestamp(event.timestamp),
                style = monoStyle(12),
                color = LognaraPalette.TextMuted,
                maxLines = 1,
            )
            Text(
                event.server,
                style = monoStyle(12),
                color = LognaraPalette.TextSecondary,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Icon(
                painterResource(R.drawable.ic_chevron_right),
                contentDescription = null,
                tint = LognaraPalette.TextMuted,
                modifier = Modifier.size(16.dp),
            )
        }
        IssueMessageText(
            text = event.message,
            modifier = Modifier.padding(start = 18.dp),
            maxLines = 2,
            styleSize = 12,
        )
    }
}

@Composable
private fun IssueSparkline(
    buckets: List<HistogramBucket>,
    color: Color,
    modifier: Modifier = Modifier,
    height: Dp,
) {
    Canvas(modifier.fillMaxWidth().height(height)) {
        val baseline = size.height - 1.dp.toPx()
        drawLine(LognaraPalette.Divider, start = androidx.compose.ui.geometry.Offset(0f, baseline), end = androidx.compose.ui.geometry.Offset(size.width, baseline))
        if (buckets.isEmpty() || size.width <= 0f || size.height <= 0f) return@Canvas

        val values = buckets.map { it.count.coerceAtLeast(0).toFloat() }
        val maximum = values.maxOrNull()?.coerceAtLeast(1f) ?: 1f
        val inset = 3.dp.toPx()
        val usableHeight = (size.height - inset * 2).coerceAtLeast(1f)
        val path = Path()
        values.forEachIndexed { index, value ->
            val x = if (values.size == 1) size.width - inset else inset + (size.width - inset * 2) * index / (values.size - 1)
            val y = size.height - inset - (value / maximum) * usableHeight
            if (index == 0) path.moveTo(x, y) else path.lineTo(x, y)
        }
        drawPath(path, color, style = Stroke(width = 1.75.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round))
        val lastValue = values.last()
        val lastY = size.height - inset - (lastValue / maximum) * usableHeight
        drawCircle(color = color, radius = 2.5.dp.toPx(), center = androidx.compose.ui.geometry.Offset(size.width - inset, lastY))
    }
}

private fun issueTileTimestamp(value: String): String = runCatching {
    DateTimeFormatter.ofPattern("dd.MM HH:mm", Locale.getDefault())
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(value))
}.getOrDefault(localTimestamp(value))
