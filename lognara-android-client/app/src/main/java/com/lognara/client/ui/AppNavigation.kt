package com.lognara.client.ui

import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
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
import com.lognara.client.ui.screens.logs.EventDetailScreen
import com.lognara.client.ui.screens.logs.TraceDetailScreen
import com.lognara.client.ui.screens.HomeScreen
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.issues.IssueDetailsScreen
import com.lognara.client.ui.screens.LoginScreen
import com.lognara.client.ui.screens.ManualEntryScreen
import com.lognara.client.ui.screens.ScannerScreen
import com.lognara.client.ui.screens.SettingsScreen
import com.lognara.client.ui.screens.CredentialsRestoreErrorScreen
import java.time.Instant
import java.time.format.DateTimeFormatter
import androidx.compose.material3.Text
import androidx.compose.ui.res.stringResource

private const val LOGIN = "login"
private const val MANUAL = "manual"
private const val SCANNER = "scanner"
private const val HOME = "home"
private const val SETTINGS = "settings"
private const val EVENT_DETAIL = "event-detail"
private const val TRACE_DETAIL = "trace-detail?traceId={traceId}&from={from}&to={to}"
private const val ISSUE_DETAIL = "issue-detail?fingerprint={fingerprint}&range={range}&service={service}"

@Composable
fun AppNavigation(viewModel: AppViewModel) {
    val session by viewModel.session.collectAsStateWithLifecycle()
    val isRestoring by viewModel.isRestoring.collectAsStateWithLifecycle()
    val credentialsRestoreError by viewModel.credentialsRestoreError.collectAsStateWithLifecycle()
    val pairing by viewModel.pairing.collectAsStateWithLifecycle()
    val settings by viewModel.settings.collectAsStateWithLifecycle()
    val notice by viewModel.loginNotice.collectAsStateWithLifecycle()
    val selectedEvent by viewModel.selectedEvent.collectAsStateWithLifecycle()
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
                        navController.navigate(EVENT_DETAIL)
                    },
                    onOpenIssue = { issue: Issue, range: DataRange, service: String ->
                        navController.navigate(
                            "issue-detail?fingerprint=${Uri.encode(issue.fingerprint)}&range=${range.name}&service=${Uri.encode(service)}",
                        )
                    },
                    issuesRefreshKey = issuesRefreshKey,
                    onIssueStatusChanged = viewModel::refreshIssues,
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
                        navController.navigate(EVENT_DETAIL)
                    },
                )
            }
        }
        composable(EVENT_DETAIL) {
            val event = selectedEvent
            if (event != null) {
                EventDetailScreen(
                    event = event,
                    onBack = {
                        viewModel.selectEvent(null)
                        navController.popBackStack()
                    },
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
                        navController.navigate(EVENT_DETAIL)
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
                )
            }
        }
    }
}
