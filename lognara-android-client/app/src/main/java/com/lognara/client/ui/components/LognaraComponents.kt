package com.lognara.client.ui.components

import androidx.annotation.DrawableRes
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.lognara.client.R
import com.lognara.client.data.LogLevel
import com.lognara.client.ui.theme.LognaraMono
import com.lognara.client.ui.theme.LognaraPalette
import com.lognara.client.ui.theme.LognaraSemanticColors

/*
 * Общие элементы редизайна по макетам `lognara-android-design`.
 * Размеры и цвета соответствуют макетам 390×844 (1 px макета = 1 dp).
 */

val CardShape = RoundedCornerShape(20.dp)
val FieldShape = RoundedCornerShape(12.dp)
val PillShape = RoundedCornerShape(999.dp)

/** Подпись секции: 11sp, заглавные буквы, разрядка 0.07em. */
val SectionLabelStyle = TextStyle(
    fontSize = 11.sp,
    lineHeight = 16.sp,
    fontWeight = FontWeight.W600,
    letterSpacing = 0.77.sp,
)

/** Моноширинный текст значений и машинных данных. */
fun monoStyle(size: Int = 13, weight: FontWeight = FontWeight.W400) = TextStyle(
    fontFamily = LognaraMono,
    fontSize = size.sp,
    lineHeight = (size * 1.5f).sp,
    fontWeight = weight,
)

/** Заголовок раздела нижней навигации: крупный title, подзаголовок и круглые кнопки справа. */
@Composable
fun ScreenHeader(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    subtitleContent: (@Composable () -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier = modifier.fillMaxWidth().padding(start = 20.dp, end = 16.dp, top = 28.dp, bottom = 14.dp),
        verticalAlignment = Alignment.Bottom,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                title,
                fontSize = 28.sp,
                lineHeight = 34.sp,
                fontWeight = FontWeight.W600,
                letterSpacing = (-0.28).sp,
                color = LognaraPalette.Text,
                modifier = Modifier.semantics { heading() },
            )
            when {
                subtitleContent != null -> subtitleContent()
                subtitle != null -> Text(
                    subtitle,
                    fontSize = 12.sp,
                    lineHeight = 18.sp,
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), content = actions)
    }
}

/** Заголовок вложенного экрана: «назад», title 16sp и необязательные действия. */
@Composable
fun DetailTopBar(
    title: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier = modifier.fillMaxWidth().padding(start = 12.dp, end = 16.dp, top = 16.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(
            modifier = Modifier
                .size(48.dp)
                .clip(CircleShape)
                .clickable(role = Role.Button, onClick = onBack),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                painterResource(R.drawable.ic_arrow_back),
                contentDescription = stringResource(R.string.back),
                tint = LognaraPalette.Text,
                modifier = Modifier.size(22.dp),
            )
        }
        Text(
            title,
            fontSize = 16.sp,
            fontWeight = FontWeight.W600,
            color = LognaraPalette.Text,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f).semantics { heading() },
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), content = actions)
    }
}

/** Круглая кнопка 48dp с рамкой: настройки, обновление, копирование. */
@Composable
fun RoundIconButton(
    @DrawableRes icon: Int,
    contentDescription: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    Box(
        modifier = modifier
            .size(48.dp)
            .clip(CircleShape)
            .background(LognaraPalette.Surface)
            .border(1.dp, LognaraPalette.BorderStrong, CircleShape)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            painterResource(icon),
            contentDescription = contentDescription,
            tint = if (enabled) LognaraPalette.TextSecondary else LognaraPalette.TextMuted.copy(alpha = 0.5f),
            modifier = Modifier.size(20.dp),
        )
    }
}

/** Кнопка настроек для заголовков разделов. */
@Composable
fun SettingsButton(onClick: () -> Unit) {
    RoundIconButton(R.drawable.ic_sliders, stringResource(R.string.open_settings), onClick)
}

/** Карточка: фон surface, рамка 8% и радиус 20dp. */
@Composable
fun LognaraCard(
    modifier: Modifier = Modifier,
    contentPadding: PaddingValues = PaddingValues(16.dp),
    shape: RoundedCornerShape = CardShape,
    onClick: (() -> Unit)? = null,
    verticalArrangement: Arrangement.Vertical = Arrangement.spacedBy(12.dp),
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(LognaraPalette.Surface)
            .border(1.dp, LognaraPalette.Border, shape)
            .then(if (onClick != null) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier)
            .padding(contentPadding),
        verticalArrangement = verticalArrangement,
        content = content,
    )
}

/** Тонкий разделитель строк внутри карточки. */
@Composable
fun RowDivider(modifier: Modifier = Modifier) {
    HorizontalDivider(modifier = modifier, thickness = 1.dp, color = LognaraPalette.Divider)
}

/** Подпись секции заглавными буквами. */
@Composable
fun SectionLabel(text: String, modifier: Modifier = Modifier, color: Color = LognaraPalette.TextMuted) {
    Text(text.uppercase(), style = SectionLabelStyle, color = color, modifier = modifier, maxLines = 1)
}

/** Сегментированный переключатель в капсуле; выбранный пункт залит акцентом. */
@Composable
fun <T> SegmentedSelector(
    options: List<T>,
    selected: T,
    label: @Composable (T) -> String,
    onSelect: (T) -> Unit,
    modifier: Modifier = Modifier,
    itemHeight: Dp = 36.dp,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(PillShape)
            .background(LognaraPalette.Surface)
            .border(1.dp, LognaraPalette.BorderStrong, PillShape)
            .padding(4.dp)
            .selectableGroup(),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        options.forEach { option ->
            val isSelected = option == selected
            Box(
                modifier = Modifier
                    .weight(1f)
                    .height(itemHeight)
                    .clip(PillShape)
                    .background(if (isSelected) LognaraPalette.Accent else Color.Transparent)
                    .selectable(selected = isSelected, role = Role.Tab, onClick = { onSelect(option) }),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    label(option),
                    fontSize = 14.sp,
                    fontWeight = if (isSelected) FontWeight.W600 else FontWeight.W500,
                    color = if (isSelected) LognaraPalette.OnAccent else LognaraPalette.TextSecondary,
                    maxLines = 1,
                )
            }
        }
    }
}

/** Ряд фильтров с горизонтальной прокруткой, выходящий за боковые отступы экрана. */
@Composable
fun ChipRow(
    modifier: Modifier = Modifier,
    horizontalPadding: Dp = 16.dp,
    content: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = horizontalPadding),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
        content = content,
    )
}

/**
 * Капсула фильтра высотой 40dp. `selected` заливает её акцентом;
 * `leading` — точка уровня, иконка и т. п.
 */
@Composable
fun FilterPill(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    selected: Boolean = false,
    mono: Boolean = false,
    textColor: Color? = null,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .height(40.dp)
            .clip(PillShape)
            .background(if (selected) LognaraPalette.Accent else LognaraPalette.Surface)
            .border(1.dp, if (selected) Color.Transparent else LognaraPalette.BorderStrong, PillShape)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        leading?.invoke()
        Text(
            text,
            style = if (mono) monoStyle(12, FontWeight.W500).copy(letterSpacing = 0.5.sp) else TextStyle(fontSize = 14.sp, fontWeight = FontWeight.W500),
            color = textColor ?: if (selected) LognaraPalette.OnAccent else LognaraPalette.TextSecondary,
            maxLines = 1,
        )
        trailing?.invoke()
    }
}

/** Точка уровня в фильтре: залитая для включённого, контурная для выключенного. */
@Composable
fun LevelDot(color: Color, filled: Boolean, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .size(8.dp)
            .clip(CircleShape)
            .background(if (filled) color else Color.Transparent)
            .border(1.5.dp, color, CircleShape),
    )
}

/** Цвет уровня лога из фиксированной палитры. */
@Composable
fun levelColor(level: LogLevel): Color {
    val colors = LognaraSemanticColors
    return when (level) {
        LogLevel.TRACE -> colors.trace
        LogLevel.DEBUG -> colors.debug
        LogLevel.INFO -> colors.info
        LogLevel.WARN -> colors.warn
        LogLevel.ERROR -> colors.error
        LogLevel.FATAL -> colors.fatal
        LogLevel.UNKNOWN -> colors.unknown
    }
}

/** Бейдж уровня лога: моноширинный текст в капсуле с подложкой цвета уровня. */
@Composable
fun LevelBadge(level: LogLevel, modifier: Modifier = Modifier) {
    val color = levelColor(level)
    val (background, foreground) = when (level) {
        LogLevel.TRACE, LogLevel.DEBUG -> LognaraPalette.TextMuted.copy(alpha = 0.15f) to LognaraPalette.TextMuted
        LogLevel.UNKNOWN -> LognaraPalette.Divider to LognaraPalette.TextMuted
        else -> color.copy(alpha = 0.15f) to color
    }
    Box(
        modifier = modifier.height(20.dp).clip(PillShape).background(background).padding(horizontal = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            level.name,
            style = monoStyle(11, FontWeight.W500).copy(letterSpacing = 0.44.sp, lineHeight = 14.sp),
            color = foreground,
            maxLines = 1,
        )
    }
}

/** Капсула статуса с точкой: «В сети», «Открыта», «Устройство подключено». */
@Composable
fun StatusPill(text: String, color: Color, modifier: Modifier = Modifier, height: Dp = 24.dp) {
    Row(
        modifier = modifier
            .height(height)
            .clip(PillShape)
            .background(color.copy(alpha = 0.12f))
            .padding(horizontal = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Box(Modifier.size(6.dp).clip(CircleShape).background(color))
        Text(text, fontSize = 12.sp, fontWeight = FontWeight.W600, color = color, maxLines = 1)
    }
}

/** Нейтральная капсула-метка (сервис, окружение). */
@Composable
fun TagPill(text: String, modifier: Modifier = Modifier, mono: Boolean = false) {
    Box(
        modifier = modifier
            .height(24.dp)
            .clip(PillShape)
            .border(1.dp, LognaraPalette.BorderStrong, PillShape)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text,
            style = if (mono) monoStyle(12) else TextStyle(fontSize = 12.sp, fontWeight = FontWeight.W500),
            color = LognaraPalette.TextSecondary,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Поле поиска высотой 48dp с иконкой лупы и моноширинным вводом. */
@Composable
fun SearchField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    onSearch: (() -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .height(48.dp)
            .clip(FieldShape)
            .background(LognaraPalette.Surface)
            .border(1.dp, LognaraPalette.BorderStrong, FieldShape)
            .padding(horizontal = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(
            painterResource(R.drawable.ic_search),
            contentDescription = null,
            tint = LognaraPalette.TextMuted,
            modifier = Modifier.size(18.dp),
        )
        Box(Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
            if (value.isEmpty()) {
                Text(placeholder, style = monoStyle(14), color = LognaraPalette.TextMuted.copy(alpha = 0.7f), maxLines = 1)
            }
            BasicTextField(
                value = value,
                onValueChange = onValueChange,
                singleLine = true,
                textStyle = monoStyle(14).copy(color = LognaraPalette.Text),
                cursorBrush = SolidColor(LognaraPalette.Accent),
                keyboardOptions = KeyboardOptions(imeAction = if (onSearch != null) ImeAction.Search else ImeAction.Done),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(onSearch = { onSearch?.invoke() }),
                modifier = Modifier.fillMaxWidth().semantics { contentDescription = placeholder },
            )
        }
        trailing?.invoke()
    }
}

/** Небольшая моноширинная подсказка внутри поля (`service:`). */
@Composable
fun FieldHint(text: String) {
    Box(
        modifier = Modifier
            .height(24.dp)
            .clip(RoundedCornerShape(6.dp))
            .border(1.dp, LognaraPalette.BorderStrong, RoundedCornerShape(6.dp))
            .padding(horizontal = 6.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = monoStyle(11), color = LognaraPalette.TextMuted)
    }
}

/**
 * Строка события в списке логов и Live: уровень, время, сервис, сервер справа
 * и сообщение до двух строк моноширинным шрифтом.
 */
@Composable
fun LogEventRow(
    level: LogLevel,
    time: String,
    service: String,
    server: String,
    message: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    highlighted: Boolean = false,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(if (highlighted) LognaraPalette.Accent.copy(alpha = 0.08f) else Color.Transparent)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            LevelBadge(level)
            Text(time, style = monoStyle(13), color = LognaraPalette.TextMuted, maxLines = 1)
            Text(
                service,
                fontSize = 13.sp,
                fontWeight = FontWeight.W600,
                color = LognaraPalette.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            Spacer(Modifier.weight(1f))
            Text(
                server,
                style = monoStyle(12),
                color = LognaraPalette.TextMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
        }
        Text(
            message,
            style = monoStyle(13),
            color = LognaraPalette.Text,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Крупная кнопка-капсула 56dp: основная (акцент) или контурная. */
@Composable
fun PillButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    primary: Boolean = false,
    enabled: Boolean = true,
    contentColor: Color? = null,
    borderColor: Color? = null,
    @DrawableRes icon: Int? = null,
) {
    val foreground = contentColor ?: if (primary) LognaraPalette.OnAccent else LognaraPalette.Text
    Row(
        modifier = modifier
            .heightIn(min = 56.dp)
            .clip(PillShape)
            .background(if (primary) LognaraPalette.Accent else LognaraPalette.Surface)
            .border(1.dp, borderColor ?: if (primary) Color.Transparent else LognaraPalette.BorderStrong, PillShape)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 18.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
    ) {
        if (icon != null) {
            Icon(painterResource(icon), contentDescription = null, tint = foreground, modifier = Modifier.size(18.dp))
        }
        Text(
            text,
            fontSize = 15.sp,
            fontWeight = FontWeight.W600,
            color = if (enabled) foreground else foreground.copy(alpha = 0.5f),
            maxLines = 1,
        )
    }
}

/** Пункт нижней навигации. */
data class BottomNavItem(@DrawableRes val icon: Int, val label: String, val badge: Long? = null)

/** Нижняя навигация макета: капсула 60×32 под активной иконкой и красный бейдж счётчика. */
@Composable
fun LognaraBottomBar(items: List<BottomNavItem>, selected: Int, onSelect: (Int) -> Unit) {
    Column(Modifier.fillMaxWidth().background(LognaraPalette.Surface)) {
        RowDivider()
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(start = 4.dp, end = 4.dp, top = 6.dp, bottom = 6.dp)
                .selectableGroup(),
        ) {
            items.forEachIndexed { index, item ->
                val isSelected = index == selected
                Column(
                    modifier = Modifier
                        .weight(1f)
                        .selectable(
                            selected = isSelected,
                            role = Role.Tab,
                            interactionSource = remember { MutableInteractionSource() },
                            indication = ripple(bounded = false, radius = 36.dp),
                            onClick = { onSelect(index) },
                        )
                        .padding(top = 6.dp, bottom = 4.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Box(Modifier.width(60.dp).height(32.dp)) {
                        Box(
                            modifier = Modifier
                                .matchParentSize()
                                .clip(PillShape)
                                .background(if (isSelected) LognaraPalette.AccentSoft else Color.Transparent),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(
                                painterResource(item.icon),
                                contentDescription = null,
                                tint = if (isSelected) LognaraPalette.Accent else LognaraPalette.TextMuted,
                                modifier = Modifier.size(22.dp),
                            )
                        }
                        item.badge?.takeIf { it > 0 }?.let { count ->
                            CountBadge(count, Modifier.align(Alignment.TopEnd).offset(x = (-4).dp, y = (-6).dp))
                        }
                    }
                    Text(
                        item.label,
                        fontSize = 12.sp,
                        fontWeight = if (isSelected) FontWeight.W600 else FontWeight.W500,
                        color = if (isSelected) LognaraPalette.Text else LognaraPalette.TextMuted,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}

/** Красный бейдж счётчика у иконки навигации. */
@Composable
fun CountBadge(count: Long, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .size(20.dp)
            .clip(CircleShape)
            .background(LognaraSemanticColors.error)
            .border(2.dp, LognaraPalette.Surface, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            if (count > 99) "99" else count.toString(),
            fontSize = 11.sp,
            lineHeight = 12.sp,
            fontWeight = FontWeight.W700,
            color = LognaraPalette.OnAccent,
        )
    }
}

/** Пара «подпись — значение» для сеток статистики. */
@Composable
fun StatValue(label: String, value: String, modifier: Modifier = Modifier, mono: Boolean = false) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(label, fontSize = 13.sp, color = LognaraPalette.TextMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text(
            value,
            style = if (mono) monoStyle(15, FontWeight.W500) else TextStyle(fontSize = 16.sp, fontWeight = FontWeight.W600),
            color = LognaraPalette.Text,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Плитка с подписью заглавными и крупным значением (детали проблемы и метрики). */
@Composable
fun StatTile(label: String, value: String, modifier: Modifier = Modifier, mono: Boolean = false) {
    Column(
        modifier = modifier
            .clip(RoundedCornerShape(16.dp))
            .background(LognaraPalette.Surface)
            .border(1.dp, LognaraPalette.Border, RoundedCornerShape(16.dp))
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        SectionLabel(label)
        Text(
            value,
            style = if (mono) monoStyle(16, FontWeight.W500) else TextStyle(fontSize = 24.sp, lineHeight = 30.sp, fontWeight = FontWeight.W600),
            color = LognaraPalette.Text,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Полоса заполнения (WAL, хранилище, спул, серверы проблемы). */
@Composable
fun UsageBar(fraction: Float, color: Color, modifier: Modifier = Modifier, height: Dp = 6.dp) {
    Box(
        modifier = modifier
            .fillMaxWidth()
            .height(height)
            .clip(PillShape)
            .background(LognaraPalette.Divider.copy(alpha = 0.5f).compositeOn(LognaraPalette.Background)),
    ) {
        Box(
            Modifier
                .fillMaxWidth(fraction.coerceIn(0f, 1f))
                .height(height)
                .clip(PillShape)
                .background(color),
        )
    }
}

private fun Color.compositeOn(background: Color): Color {
    val a = alpha
    return Color(
        red = red * a + background.red * (1 - a),
        green = green * a + background.green * (1 - a),
        blue = blue * a + background.blue * (1 - a),
        alpha = 1f,
    )
}
