package com.lognara.client.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.shape.RoundedCornerShape

data class LognaraColors(
    val trace: Color,
    val debug: Color,
    val info: Color,
    val warn: Color,
    val error: Color,
    val fatal: Color,
    val unknown: Color,
    val statusOk: Color,
    val statusWarn: Color,
    val statusErr: Color,
    val statusInfo: Color,
)

val LocalLognaraColors = staticCompositionLocalOf {
    LognaraColors(
        trace = Color(0xFF5A534D), debug = Color(0xFF7A7068), info = Color(0xFF9AA6E8),
        warn = Color(0xFFC07A52), error = Color(0xFFF87171), fatal = Color(0xFFD9466B),
        unknown = Color(0xFF4A443F), statusOk = Color(0xFF34D399), statusWarn = Color(0xFFD9966F),
        statusErr = Color(0xFFF87171), statusInfo = Color(0xFF9AA6E8),
    )
}

val LognaraSemanticColors: LognaraColors
    @Composable get() = LocalLognaraColors.current

@Composable
fun LognaraTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    val baseScheme = if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
    val scheme = if (dark) {
        baseScheme.copy(
            primary = Color(0xFFC07A52), onPrimary = Color(0xFF0B0908),
            background = Color(0xFF141210), surface = Color(0xFF1C1916),
            surfaceVariant = Color(0xFF25211D), onBackground = Color(0xFFE5DFD7),
            onSurface = Color(0xFFE5DFD7), onSurfaceVariant = Color(0xFFC9C1B8),
            outline = Color(0xFF6B625A), error = Color(0xFFF87171), onError = Color(0xFF2A0808),
        )
    } else {
        baseScheme.copy(
            primary = Color(0xFF92512F), onPrimary = Color.White,
            background = Color(0xFFF7F4F0), surface = Color(0xFFFFFBF7),
            surfaceVariant = Color(0xFFEFEAE4), onBackground = Color(0xFF211A16),
            onSurface = Color(0xFF211A16), onSurfaceVariant = Color(0xFF514943),
            outline = Color(0xFF756C64), error = Color(0xFFB3261E), onError = Color.White,
        )
    }
    val semanticColors = if (dark) {
        LognaraColors(
            trace = Color(0xFF5A534D), debug = Color(0xFF7A7068), info = Color(0xFF9AA6E8),
            warn = Color(0xFFC07A52), error = Color(0xFFF87171), fatal = Color(0xFFD9466B),
            unknown = Color(0xFF4A443F), statusOk = Color(0xFF34D399), statusWarn = Color(0xFFD9966F),
            statusErr = Color(0xFFF87171), statusInfo = Color(0xFF9AA6E8),
        )
    } else {
        LognaraColors(
            trace = Color(0xFF645B54), debug = Color(0xFF5C5148), info = Color(0xFF4956A8),
            warn = Color(0xFF86512D), error = Color(0xFFB3261E), fatal = Color(0xFFA02045),
            unknown = Color(0xFF5C5249), statusOk = Color(0xFF1B6B49), statusWarn = Color(0xFF7E4A24),
            statusErr = Color(0xFFB3261E), statusInfo = Color(0xFF3E4F9B),
        )
    }

    CompositionLocalProvider(LocalLognaraColors provides semanticColors) {
        MaterialTheme(
            colorScheme = scheme,
            typography = LognaraTypography,
            shapes = Shapes(
                extraSmall = RoundedCornerShape(8.dp),
                small = RoundedCornerShape(10.dp),
                medium = RoundedCornerShape(12.dp),
                large = RoundedCornerShape(14.dp),
                extraLarge = RoundedCornerShape(20.dp),
            ),
            content = content,
        )
    }
}
