package com.lognara.client.ui.screens.issues

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.Issue
import com.lognara.client.data.IssueDetailsResponse
import com.lognara.client.data.IssueStatus
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LognaraApi
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.RangeSelector
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.MachineTextStyle

@Composable
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
    androidx.compose.foundation.layout.Column(Modifier.fillMaxSize()) {
        com.lognara.client.ui.components.ScreenHeader(
            title = stringResource(R.string.tab_issues),
            actions = { com.lognara.client.ui.components.SettingsButton(onSettings) },
        )
        Box(Modifier.weight(1f)) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(refreshKey) {
        if (refreshKey > 0) viewModel.reload()
    }

    Column(Modifier.fillMaxSize().padding(horizontal = 12.dp)) {
        val content = (state.result as? DataLoadState.Content)?.value
        LazyRow(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
            items(IssueStatus.entries, key = { it.name }) { status ->
                FilterChip(
                    selected = state.status == status,
                    onClick = { viewModel.setStatus(status) },
                    label = {
                        Text(
                            when (status) {
                                IssueStatus.OPEN -> stringResource(R.string.issue_status_open, content?.response?.counts?.open ?: 0)
                                IssueStatus.NEW -> content?.response?.counts?.new?.let {
                                    stringResource(R.string.issue_status_new, it)
                                } ?: stringResource(R.string.issue_status_new_unknown)
                                IssueStatus.REGRESSED -> stringResource(R.string.issue_status_regressed, content?.response?.counts?.regressed ?: 0)
                                IssueStatus.RESOLVED -> stringResource(R.string.issue_status_resolved, content?.response?.counts?.resolved ?: 0)
                            },
                        )
                    },
                )
            }
        }
        RangeSelector(selected = state.range, onSelect = viewModel::setRange)
        IssueServiceSelector(
            selected = state.service,
            services = content?.services.orEmpty(),
            onSelect = viewModel::setService,
        )
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            TextButton(onClick = viewModel::reload) { Text(stringResource(R.string.issue_refresh)) }
        }
        DataStateContent(
            state = state.result,
            isEmpty = { false },
            onRetry = viewModel::reload,
            modifier = Modifier.weight(1f).fillMaxWidth(),
        ) { data ->
            if (data.response.issues.isEmpty()) {
                Text(
                    text = if (state.status == IssueStatus.NEW && data.response.counts.new == null) {
                        stringResource(R.string.issue_new_unknown)
                    } else {
                        stringResource(R.string.issue_empty)
                    },
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.fillMaxWidth().padding(24.dp),
                )
            } else {
                LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxSize()) {
                    items(data.response.issues, key = { it.fingerprint }) { issue ->
                        IssueCard(issue) { onOpenIssue(issue, state.range, state.service) }
                    }
                }
            }
        }
    }
        }
    }
}

@Composable
private fun IssueServiceSelector(selected: String, services: List<String>, onSelect: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box(Modifier.fillMaxWidth()) {
        OutlinedButton(onClick = { expanded = true }, modifier = Modifier.fillMaxWidth()) {
            Text(selected.ifBlank { stringResource(R.string.issue_service_all) })
        }
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
private fun IssueCard(issue: Issue, onClick: () -> Unit) {
    Card(modifier = Modifier.fillMaxWidth().clickable(onClick = onClick)) {
        Column(Modifier.fillMaxWidth().padding(14.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(issue.exception_type ?: issue.template, style = MaterialTheme.typography.titleMedium)
            Text(
                text = listOfNotNull(issue.service, issue.action).joinToString(" · "),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(issueStatusLabel(issue), color = MaterialTheme.colorScheme.primary)
            Text(stringResource(R.string.issue_count, issue.count, issue.servers))
            Text(stringResource(R.string.issue_last_seen, localTimestamp(issue.last_seen)))
        }
    }
}

@Composable
private fun issueStatusLabel(issue: Issue): String = when {
    issue.state?.status == "resolved" -> stringResource(R.string.issue_state_resolved)
    issue.state?.status == "regressed" -> stringResource(R.string.issue_state_regressed)
    issue.is_new == true -> stringResource(R.string.issue_state_new)
    else -> stringResource(R.string.issue_state_open)
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
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

    Scaffold(
        topBar = {
            CenterAlignedTopAppBar(
                title = { Text(stringResource(R.string.issue_detail_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = stringResource(R.string.back))
                    }
                },
            )
        },
    ) { insets ->
        DataStateContent(
            state = state.result,
            isEmpty = { false },
            onRetry = viewModel::retry,
            modifier = Modifier.fillMaxSize().padding(insets).padding(horizontal = 16.dp),
        ) { details ->
            IssueDetailsContent(
                details = details,
                actionPending = state.actionPending,
                actionError = state.actionError,
                onRequestAction = { confirmAction = it },
                onOpenEvent = onOpenEvent,
            )
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
    actionPending: Boolean,
    actionError: Boolean,
    onRequestAction: (IssueAction) -> Unit,
    onOpenEvent: (LogEvent) -> Unit,
) {
    val resolve = details.state.status != "resolved"
    Column(
        Modifier.fillMaxSize().padding(vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        LazyColumn(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            item {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(details.issue.exception_type ?: details.issue.template, style = MaterialTheme.typography.titleLarge)
                    Text(
                        text = listOfNotNull(details.issue.service, details.issue.action).joinToString(" · "),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(issueStatusLabel(details.issue.copy(state = details.state)), color = MaterialTheme.colorScheme.primary)
                    Text(stringResource(R.string.issue_count, details.issue.count, details.issue.servers))
                    Text(stringResource(R.string.issue_first_seen, localTimestamp(details.issue.first_seen)))
                    Text(stringResource(R.string.issue_last_seen, localTimestamp(details.issue.last_seen)))
                    Text(stringResource(R.string.issue_fingerprint, details.issue.fingerprint), style = MachineTextStyle)
                    details.issue.environment?.let { Text(stringResource(R.string.issue_environment, it)) }
                }
            }
            item {
                Text(stringResource(R.string.issue_examples), style = MaterialTheme.typography.titleMedium)
            }
            if (details.examples.isEmpty()) {
                item { Text(stringResource(R.string.issue_no_examples), color = MaterialTheme.colorScheme.onSurfaceVariant) }
            } else {
                items(details.examples, key = { it.id }) { event ->
                    Card(modifier = Modifier.fillMaxWidth().clickable { onOpenEvent(event) }) {
                        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(event.message, maxLines = 4)
                            Text(
                                stringResource(R.string.issue_example_metadata, event.server, localTimestamp(event.timestamp)),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                style = MachineTextStyle,
                            )
                        }
                    }
                }
            }
            item { Text(stringResource(R.string.issue_servers), style = MaterialTheme.typography.titleMedium) }
            if (details.servers.isEmpty()) {
                item { Text(stringResource(R.string.issue_no_servers), color = MaterialTheme.colorScheme.onSurfaceVariant) }
            } else {
                items(details.servers, key = { it.value }) { server ->
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                        Text(server.value, style = MachineTextStyle)
                        Text(server.count.toString())
                    }
                }
                if (details.other_servers > 0) {
                    item { Text(stringResource(R.string.issue_other_servers, details.other_servers)) }
                }
            }
            item { Text(stringResource(R.string.issue_history), style = MaterialTheme.typography.titleMedium) }
            item {
                if (details.history.isEmpty()) {
                    Text(stringResource(R.string.issue_no_history), color = MaterialTheme.colorScheme.onSurfaceVariant)
                } else {
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        details.history.forEach { item ->
                            Text(
                                stringResource(
                                    when (item.action) {
                                        "resolved" -> R.string.issue_history_resolved
                                        "reopened" -> R.string.issue_history_reopened
                                        else -> R.string.issue_history_regressed
                                    },
                                    localTimestamp(item.at),
                                    item.actor ?: stringResource(R.string.issue_actor_system),
                                ),
                            )
                        }
                    }
                }
            }
        }
        if (actionError) Text(stringResource(R.string.issue_action_error), color = MaterialTheme.colorScheme.error)
        Button(
            enabled = !actionPending,
            onClick = { onRequestAction(if (resolve) IssueAction.Resolve else IssueAction.Reopen) },
            modifier = Modifier.fillMaxWidth(),
        ) {
            if (actionPending) {
                CircularProgressIndicator()
                Text(stringResource(R.string.issue_action_loading), modifier = Modifier.padding(start = 8.dp))
            } else {
                Text(stringResource(if (resolve) R.string.issue_resolve else R.string.issue_reopen))
            }
        }
    }
}
