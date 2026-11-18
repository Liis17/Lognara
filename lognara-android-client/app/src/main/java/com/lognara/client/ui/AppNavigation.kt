package com.lognara.client.ui

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import android.widget.Toast
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.Issue
import com.lognara.client.data.MetricKey
import com.lognara.client.data.SeriesMetric
import com.lognara.client.data.UpdateChannel
import com.lognara.client.ui.screens.logs.EventDetailScreen
import com.lognara.client.ui.screens.logs.TraceDetailScreen
import com.lognara.client.ui.screens.HomeScreen
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.issues.IssueDetailsScreen
import com.lognara.client.ui.screens.metrics.MetricDetailsScreen
import com.lognara.client.ui.screens.LoginScreen
import com.lognara.client.ui.screens.ManualEntryScreen
import com.lognara.client.ui.screens.ScannerScreen
import com.lognara.client.ui.screens.SettingsScreen
import com.lognara.client.ui.screens.AppSettingsScreen
import com.lognara.client.ui.screens.CredentialsRestoreErrorScreen
import com.lognara.client.ui.theme.LognaraPalette
import java.time.Instant
import java.time.format.DateTimeFormatter

private const val LOGIN = "login"
private const val MANUAL = "manual"
private const val SCANNER = "scanner"
private const val HOME = "home"
private const val SETTINGS = "settings"
private const val APP_SETTINGS = "app-settings"
private const val EVENT_DETAIL = "event-detail?eventId={eventId}"
private const val TRACE_DETAIL = "trace-detail?traceId={traceId}&from={from}&to={to}"
private const val ISSUE_DETAIL = "issue-detail?fingerprint={fingerprint}&range={range}&service={service}"
private const val METRIC_DETAIL = "metric-detail?server={server}&service={service}&name={name}&origin={origin}"

@Composable
fun AppNavigation(viewModel: AppViewModel) {
    val session by viewModel.session.collectAsStateWithLifecycle()
    val isRestoring by viewModel.isRestoring.collectAsStateWithLifecycle()
    val credentialsRestoreError by viewModel.credentialsRestoreError.collectAsStateWithLifecycle()
    val pairing by viewModel.pairing.collectAsStateWithLifecycle()
    val settings by viewModel.settings.collectAsStateWithLifecycle()
    val appSettings by viewModel.appSettings.collectAsStateWithLifecycle()
    val appUpdate by viewModel.appUpdate.collectAsStateWithLifecycle()
    val showUpdatePrompt by viewModel.showUpdatePrompt.collectAsStateWithLifecycle()
    val notice by viewModel.loginNotice.collectAsStateWithLifecycle()
    val selectedEvents by viewModel.selectedEvents.collectAsStateWithLifecycle()
    val issuesRefreshKey by viewModel.issuesRefreshKey.collectAsStateWithLifecycle()

    if (isRestoring) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }
    if (credentialsRestoreError) {
        CredentialsRestoreErrorScreen(onRetry = viewModel::retryRestoreCredentials)
        return
    }

    val navController = rememberNavController()
    var previousSessionId by remember { mutableStateOf(session?.deviceId) }
    LaunchedEffect(session?.deviceId) {
        val currentSessionId = session?.deviceId
        if (previousSessionId != currentSessionId) {
            previousSessionId = currentSessionId
            if (session != null) {
                navController.navigate(HOME) {
                    popUpTo(navController.graph.findStartDestination().id) { inclusive = true }
                    launchSingleTop = true
                }
            } else {
                navController.navigate(LOGIN) {
                    popUpTo(navController.graph.id) { inclusive = true }
                    launchSingleTop = true
                }
            }
        }
    }

    NavHost(
        navController = navController,
        startDestination = if (session == null) LOGIN else HOME,
    ) {
        composable(LOGIN) {
            LoginScreen(
                pairing = pairing,
                notice = notice == LoginNotice.DeviceStillListed,
                onScan = {
                    viewModel.clearPairingError()
                    navController.navigate(SCANNER)
                },
                onManual = {
                    viewModel.clearPairingError()
                    navController.navigate(MANUAL)
                },
                onClearError = viewModel::clearPairingError,
            )
        }
        composable(MANUAL) {
            ManualEntryScreen(
                pairing = pairing,
                onBack = { navController.popBackStack() },
                onSubmit = viewModel::connect,
            )
        }
        composable(SCANNER) {
            ScannerScreen(
                pairing = pairing,
                onBack = { navController.popBackStack() },
                onPayload = viewModel::connect,
            )
        }
        composable(HOME) {
            session?.let { activeSession ->
                HomeScreen(
                    session = activeSession,
                    api = viewModel.container.api,
                    onSettings = { navController.navigate(SETTINGS) },
                    onOpenEvent = { event ->
                        viewModel.selectEvent(event)
                        navController.navigate("event-detail?eventId=${Uri.encode(event.id)}")
                    },
                    onOpenIssue = { issue: Issue, range: DataRange, service: String ->
                        navController.navigate(
                            "issue-detail?fingerprint=${Uri.encode(issue.fingerprint)}&range=${range.name}&service=${Uri.encode(service)}",
                        )
                    },
                    issuesRefreshKey = issuesRefreshKey,
                    onIssueStatusChanged = viewModel::refreshIssues,
                    onOpenMetric = { metric: SeriesMetric ->
                        navController.navigate(
                            "metric-detail?server=${Uri.encode(metric.server)}&service=${Uri.encode(metric.service)}&name=${Uri.encode(metric.name)}&origin=${Uri.encode(metric.origin)}",
                        )
                    },
                )
            }
        }
        composable(
            route = METRIC_DETAIL,
            arguments = listOf(
                navArgument("server") { type = NavType.StringType; defaultValue = "" },
                navArgument("service") { type = NavType.StringType },
                navArgument("name") { type = NavType.StringType },
                navArgument("origin") { type = NavType.StringType },
            ),
        ) { entry ->
            val activeSession: DeviceSession? = session
            if (activeSession != null) {
                MetricDetailsScreen(
                    api = viewModel.container.api,
                    session = activeSession,
                    key = MetricKey(
                        server = entry.arguments?.getString("server").orEmpty(),
                        service = entry.arguments?.getString("service").orEmpty(),
                        name = entry.arguments?.getString("name").orEmpty(),
                        origin = entry.arguments?.getString("origin").orEmpty(),
                    ),
                    onBack = { navController.popBackStack() },
                )
            }
        }
        composable(
            route = ISSUE_DETAIL,
            arguments = listOf(
                navArgument("fingerprint") { type = NavType.StringType },
                navArgument("range") { type = NavType.StringType; defaultValue = DataRange.TwentyFourHours.name },
                navArgument("service") { type = NavType.StringType; defaultValue = "" },
            ),
        ) { entry ->
            val activeSession: DeviceSession? = session
            if (activeSession != null) {
                IssueDetailsScreen(
                    api = viewModel.container.api,
                    session = activeSession,
                    fingerprint = entry.arguments?.getString("fingerprint").orEmpty(),
                    range = DataRange.entries.firstOrNull { it.name == entry.arguments?.getString("range") }
                        ?: DataRange.TwentyFourHours,
                    service = entry.arguments?.getString("service").orEmpty(),
                    onBack = { navController.popBackStack() },
                    onStatusChanged = viewModel::refreshIssues,
                    onOpenEvent = { event ->
                        viewModel.selectEvent(event)
                        navController.navigate("event-detail?eventId=${Uri.encode(event.id)}")
                    },
                )
            }
        }
        composable(
            route = EVENT_DETAIL,
            arguments = listOf(navArgument("eventId") { type = NavType.StringType }),
        ) { entry ->
            val event = selectedEvents[entry.arguments?.getString("eventId").orEmpty()]
            if (event != null) {
                EventDetailScreen(
                    event = event,
                    onBack = { navController.popBackStack() },
                    onOpenTrace = { selected ->
                        val center = runCatching { Instant.parse(selected.timestamp) }.getOrDefault(Instant.now())
                        val from = DateTimeFormatter.ISO_INSTANT.format(center.minusSeconds(300))
                        val to = DateTimeFormatter.ISO_INSTANT.format(center.plusSeconds(300))
                        navController.navigate(
                            "trace-detail?traceId=${Uri.encode(selected.trace_id.orEmpty())}&from=${Uri.encode(from)}&to=${Uri.encode(to)}",
                        )
                    },
                )
            } else {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text(stringResource(R.string.event_missing))
                }
            }
        }
        composable(
            route = TRACE_DETAIL,
            arguments = listOf(
                navArgument("traceId") { type = NavType.StringType },
                navArgument("from") { type = NavType.StringType },
                navArgument("to") { type = NavType.StringType },
            ),
        ) { entry ->
            val activeSession: DeviceSession? = session
            if (activeSession != null) {
                TraceDetailScreen(
                    api = viewModel.container.api,
                    session = activeSession,
                    traceId = entry.arguments?.getString("traceId").orEmpty(),
                    from = entry.arguments?.getString("from").orEmpty(),
                    to = entry.arguments?.getString("to").orEmpty(),
                    onBack = { navController.popBackStack() },
                    onOpenEvent = { event ->
                        viewModel.selectEvent(event)
                        navController.navigate("event-detail?eventId=${Uri.encode(event.id)}")
                    },
                )
            }
        }
        composable(SETTINGS) {
            val activeSession: DeviceSession? = session
            if (activeSession != null) {
                SettingsScreen(
                    session = activeSession,
                    state = settings,
                    onBack = { navController.popBackStack() },
                    onLoad = viewModel::loadSettings,
                    onRetry = viewModel::loadSettings,
                    onDisconnect = viewModel::disconnect,
                    onOpenAppSettings = { navController.navigate(APP_SETTINGS) },
                )
            }
        }
        composable(APP_SETTINGS) {
            AppSettingsScreen(
                settings = appSettings,
                update = appUpdate,
                onBack = { navController.popBackStack() },
                onAutoCheckChanged = viewModel::setAutoCheck,
                onChannelChanged = viewModel::setUpdateChannel,
                onCheckNow = { viewModel.checkForUpdates() },
                onInstall = viewModel::installUpdate,
            )
        }
    }

    val availableUpdate = appUpdate.update
    if (showUpdatePrompt && availableUpdate != null) {
        val context = LocalContext.current
        val phase = appUpdate.phase
        AlertDialog(
            onDismissRequest = viewModel::dismissUpdatePrompt,
            title = { Text(stringResource(R.string.app_startup_update_title, availableUpdate.manifest.versionName)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        stringResource(
                            R.string.app_startup_update_channel,
                            stringResource(if (availableUpdate.manifest.channel == UpdateChannel.Stable) R.string.app_channel_stable else R.string.app_channel_dev),
                        ),
                        fontSize = 14.sp,
                    )
                    if (appUpdate.error != null) {
                        Text(appUpdate.error.orEmpty(), fontSize = 13.sp, color = LognaraPalette.TextMuted)
                    } else if (phase == UpdatePhase.Downloading) {
                        Text(
                            stringResource(R.string.app_download_in_progress, availableUpdate.manifest.versionName),
                            fontSize = 13.sp,
                            color = LognaraPalette.TextMuted,
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        if (!viewModel.installUpdate()) openUnknownSourcesSettings(context)
                    },
                    enabled = phase != UpdatePhase.Downloading && phase != UpdatePhase.Installing,
                ) {
                    Text(
                        stringResource(if (phase == UpdatePhase.PermissionRequired) R.string.app_allow_install else R.string.app_update_now),
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = viewModel::dismissUpdatePrompt) { Text(stringResource(R.string.app_later)) }
            },
            shape = androidx.compose.foundation.shape.RoundedCornerShape(28.dp),
            containerColor = LognaraPalette.Surface,
            titleContentColor = LognaraPalette.Text,
            textContentColor = LognaraPalette.TextSecondary,
        )
    }
}

private fun openUnknownSourcesSettings(context: android.content.Context) {
    try {
        context.startActivity(
            Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    } catch (_: ActivityNotFoundException) {
        Toast.makeText(context, R.string.app_unknown_sources_settings_not_found, Toast.LENGTH_LONG).show()
    }
}
