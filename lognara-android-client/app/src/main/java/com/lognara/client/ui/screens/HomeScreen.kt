package com.lognara.client.ui.screens

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LognaraApi
import com.lognara.client.ui.screens.logs.LogsScreen
import com.lognara.client.ui.screens.live.LiveScreen

private data class HomeTab(val title: Int, val symbol: TabSymbol)
private enum class TabSymbol { Logs, Live, Issues, Infrastructure, Metrics, Settings }

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun HomeScreen(
    session: DeviceSession,
    api: LognaraApi,
    onSettings: () -> Unit,
    onOpenEvent: (LogEvent) -> Unit,
) {
    val tabs = listOf(
        HomeTab(R.string.tab_logs, TabSymbol.Logs),
        HomeTab(R.string.tab_live, TabSymbol.Live),
        HomeTab(R.string.tab_issues, TabSymbol.Issues),
        HomeTab(R.string.tab_infrastructure, TabSymbol.Infrastructure),
        HomeTab(R.string.tab_metrics, TabSymbol.Metrics),
    )
    var selectedTab by rememberSaveable { mutableIntStateOf(0) }
    val activeColor = MaterialTheme.colorScheme.primary
    val inactiveColor = MaterialTheme.colorScheme.onSurfaceVariant

    Scaffold(
        topBar = {
            CenterAlignedTopAppBar(
                title = { Text(stringResource(tabs[selectedTab].title)) },
                actions = {
                    CanvasIconButton(
                        description = stringResource(R.string.open_settings),
                        onClick = onSettings,
                        symbol = TabSymbol.Settings,
                    )
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
        bottomBar = {
            NavigationBar {
                tabs.forEachIndexed { index, tab ->
                    NavigationBarItem(
                        selected = selectedTab == index,
                        onClick = { selectedTab = index },
                        icon = {
                            Canvas(
                                modifier = Modifier.size(22.dp),
                            ) {
                                drawTabSymbol(tab.symbol, if (selectedTab == index) activeColor else inactiveColor)
                            }
                        },
                        label = { Text(stringResource(tab.title)) },
                        colors = NavigationBarItemDefaults.colors(
                            selectedIconColor = activeColor,
                            selectedTextColor = activeColor,
                            indicatorColor = MaterialTheme.colorScheme.primary.copy(alpha = 0.14f),
                        ),
                    )
                }
            }
        },
    ) { insets ->
        Box(modifier = Modifier.fillMaxSize().padding(insets)) {
            if (selectedTab == 0) {
                LogsScreen(api, session, onOpenEvent)
            } else if (selectedTab == 1) {
                LiveScreen(api, session, onOpenEvent)
            } else {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text(
                        text = stringResource(R.string.section_placeholder),
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 32.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun CanvasIconButton(description: String, onClick: () -> Unit, symbol: TabSymbol) {
    val iconColor = MaterialTheme.colorScheme.onSurface
    androidx.compose.material3.IconButton(
        onClick = onClick,
        modifier = Modifier.semantics { contentDescription = description },
    ) {
        Canvas(
            modifier = Modifier.size(24.dp),
        ) {
            drawTabSymbol(symbol, iconColor)
        }
    }
}

private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawTabSymbol(symbol: TabSymbol, color: Color) {
    val stroke = Stroke(width = 1.8.dp.toPx(), cap = StrokeCap.Round)
    val w = size.width
    val h = size.height
    when (symbol) {
        TabSymbol.Logs -> {
            drawCircle(color, radius = 1.5.dp.toPx(), center = androidx.compose.ui.geometry.Offset(w * 0.2f, h * 0.27f))
            drawCircle(color, radius = 1.5.dp.toPx(), center = androidx.compose.ui.geometry.Offset(w * 0.2f, h * 0.5f))
            drawCircle(color, radius = 1.5.dp.toPx(), center = androidx.compose.ui.geometry.Offset(w * 0.2f, h * 0.73f))
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.38f, h * 0.27f), androidx.compose.ui.geometry.Offset(w * 0.82f, h * 0.27f), stroke.width, cap = StrokeCap.Round)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.38f, h * 0.5f), androidx.compose.ui.geometry.Offset(w * 0.82f, h * 0.5f), stroke.width, cap = StrokeCap.Round)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.38f, h * 0.73f), androidx.compose.ui.geometry.Offset(w * 0.7f, h * 0.73f), stroke.width, cap = StrokeCap.Round)
        }
        TabSymbol.Live -> {
            drawCircle(color, radius = w * 0.38f, style = stroke)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.5f), androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.25f), stroke.width, cap = StrokeCap.Round)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.5f), androidx.compose.ui.geometry.Offset(w * 0.7f, h * 0.62f), stroke.width, cap = StrokeCap.Round)
        }
        TabSymbol.Issues -> {
            val triangle = Path().apply {
                moveTo(w * 0.5f, h * 0.12f)
                lineTo(w * 0.92f, h * 0.86f)
                lineTo(w * 0.08f, h * 0.86f)
                close()
            }
            drawPath(triangle, color, style = stroke)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.36f), androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.59f), stroke.width, cap = StrokeCap.Round)
            drawCircle(color, 1.dp.toPx(), androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.72f))
        }
        TabSymbol.Infrastructure -> {
            val top = androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.22f)
            val left = androidx.compose.ui.geometry.Offset(w * 0.22f, h * 0.78f)
            val right = androidx.compose.ui.geometry.Offset(w * 0.78f, h * 0.78f)
            drawLine(color, top, left, stroke.width)
            drawLine(color, top, right, stroke.width)
            drawLine(color, left, right, stroke.width)
            drawCircle(color, w * 0.12f, top)
            drawCircle(color, w * 0.12f, left)
            drawCircle(color, w * 0.12f, right)
        }
        TabSymbol.Metrics -> {
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.14f, h * 0.86f), androidx.compose.ui.geometry.Offset(w * 0.14f, h * 0.14f), stroke.width)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.14f, h * 0.86f), androidx.compose.ui.geometry.Offset(w * 0.9f, h * 0.86f), stroke.width)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.3f, h * 0.74f), androidx.compose.ui.geometry.Offset(w * 0.3f, h * 0.54f), 3.dp.toPx(), cap = StrokeCap.Round)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.51f, h * 0.74f), androidx.compose.ui.geometry.Offset(w * 0.51f, h * 0.38f), 3.dp.toPx(), cap = StrokeCap.Round)
            drawLine(color, androidx.compose.ui.geometry.Offset(w * 0.72f, h * 0.74f), androidx.compose.ui.geometry.Offset(w * 0.72f, h * 0.19f), 3.dp.toPx(), cap = StrokeCap.Round)
        }
        TabSymbol.Settings -> {
            drawCircle(color, w * 0.29f, androidx.compose.ui.geometry.Offset(w * 0.5f, h * 0.5f), style = stroke)
            for (step in 0 until 8) {
                val angle = Math.toRadians(step * 45.0)
                val inner = w * 0.36f
                val outer = w * 0.46f
                val start = androidx.compose.ui.geometry.Offset(w * 0.5f + kotlin.math.cos(angle).toFloat() * inner, h * 0.5f + kotlin.math.sin(angle).toFloat() * inner)
                val end = androidx.compose.ui.geometry.Offset(w * 0.5f + kotlin.math.cos(angle).toFloat() * outer, h * 0.5f + kotlin.math.sin(angle).toFloat() * outer)
                drawLine(color, start, end, stroke.width, cap = StrokeCap.Round)
            }
        }
    }
}
