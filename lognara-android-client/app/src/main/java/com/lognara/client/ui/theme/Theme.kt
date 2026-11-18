package com.lognara.client.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

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

/** Фиксированная тёмная палитра макетов Android-клиента. */
object LognaraPalette {
    val Background = Color(0xFF141210)
    val Surface = Color(0xFF1C1916)
    val SurfaceRaised = Color(0xFF25211D)
    val Text = Color(0xFFE5DFD7)
    val TextSecondary = Color(0xFFC9C1B8)
    val TextMuted = Color(0xFFA79F97)
    val Accent = Color(0xFFC07A52)
    val OnAccent = Color(0xFF0B0908)

    /** rgba(229,223,215,.12) — рамка полей и круглых кнопок. */
    val BorderStrong = Color(0x1FE5DFD7)

    /** rgba(229,223,215,.08) — рамка карточек. */
    val Border = Color(0x14E5DFD7)

    /** rgba(229,223,215,.06) — разделители строк и линии сетки. */
    val Divider = Color(0x0FE5DFD7)

    /** rgba(192,122,82,.16) — подложка активного пункта навигации. */
    val AccentSoft = Color(0x29C07A52)
}

private val SemanticColors = LognaraColors(
    trace = Color(0xFF5A534D), debug = Color(0xFF7A7068), info = Color(0xFF9AA6E8),
    warn = Color(0xFFC07A52), error = Color(0xFFF87171), fatal = Color(0xFFD9466B),
    unknown = Color(0xFF4A443F), statusOk = Color(0xFF34D399), statusWarn = Color(0xFFD9966F),
    statusErr = Color(0xFFF87171), statusInfo = Color(0xFF9AA6E8),
)

val LocalLognaraColors = staticCompositionLocalOf { SemanticColors }

val LognaraSemanticColors: LognaraColors
    @Composable get() = LocalLognaraColors.current

private val LognaraColorScheme = darkColorScheme(
    primary = LognaraPalette.Accent,
    onPrimary = LognaraPalette.OnAccent,
    primaryContainer = LognaraPalette.AccentSoft,
    onPrimaryContainer = LognaraPalette.Accent,
    secondary = LognaraPalette.TextSecondary,
    onSecondary = LognaraPalette.OnAccent,
    secondaryContainer = LognaraPalette.SurfaceRaised,
    onSecondaryContainer = LognaraPalette.Text,
    background = LognaraPalette.Background,
    onBackground = LognaraPalette.Text,
    surface = LognaraPalette.Surface,
    onSurface = LognaraPalette.Text,
    surfaceVariant = LognaraPalette.SurfaceRaised,
    onSurfaceVariant = LognaraPalette.TextMuted,
    surfaceContainerLowest = LognaraPalette.Background,
    surfaceContainerLow = LognaraPalette.Surface,
    surfaceContainer = LognaraPalette.Surface,
    surfaceContainerHigh = LognaraPalette.SurfaceRaised,
    surfaceContainerHighest = LognaraPalette.SurfaceRaised,
    inverseSurface = LognaraPalette.Text,
    inverseOnSurface = LognaraPalette.Background,
    outline = Color(0xFF6B625A),
    outlineVariant = Color(0xFF2E2925),
    error = Color(0xFFF87171),
    onError = Color(0xFF2A0808),
)

/** Приложение использует только тёмную тему макетов, без dynamic color. */
@Composable
fun LognaraTheme(content: @Composable () -> Unit) {
    CompositionLocalProvider(LocalLognaraColors provides SemanticColors) {
        MaterialTheme(
            colorScheme = LognaraColorScheme,
            typography = LognaraTypography,
            shapes = Shapes(
                extraSmall = RoundedCornerShape(8.dp),
                small = RoundedCornerShape(10.dp),
                medium = RoundedCornerShape(12.dp),
                large = RoundedCornerShape(16.dp),
                extraLarge = RoundedCornerShape(20.dp),
            ),
            content = content,
        )
    }
}
