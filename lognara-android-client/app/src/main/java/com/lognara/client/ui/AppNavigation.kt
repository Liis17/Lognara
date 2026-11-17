package com.lognara.client.ui

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
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.lognara.client.data.DeviceSession
import com.lognara.client.ui.screens.HomeScreen
import com.lognara.client.ui.screens.LoginScreen
import com.lognara.client.ui.screens.ManualEntryScreen
import com.lognara.client.ui.screens.ScannerScreen
import com.lognara.client.ui.screens.SettingsScreen
import com.lognara.client.ui.screens.CredentialsRestoreErrorScreen

private const val LOGIN = "login"
private const val MANUAL = "manual"
private const val SCANNER = "scanner"
private const val HOME = "home"
private const val SETTINGS = "settings"

@Composable
fun AppNavigation(viewModel: AppViewModel) {
    val session by viewModel.session.collectAsStateWithLifecycle()
    val isRestoring by viewModel.isRestoring.collectAsStateWithLifecycle()
    val credentialsRestoreError by viewModel.credentialsRestoreError.collectAsStateWithLifecycle()
    val pairing by viewModel.pairing.collectAsStateWithLifecycle()
    val settings by viewModel.settings.collectAsStateWithLifecycle()
    val notice by viewModel.loginNotice.collectAsStateWithLifecycle()

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
            HomeScreen(onSettings = { navController.navigate(SETTINGS) })
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
