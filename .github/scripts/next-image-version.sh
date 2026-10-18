#!/usr/bin/env bash
# Читает теги lognara-$COMPONENT из реестра и увеличивает patch наибольшей версии X.Y.Z.
# Отсутствующий репозиторий (404 NAME_UNKNOWN) или отсутствие версий означает 0.0.1.
#
# Вход (env): COMPONENT, REGISTRY_USERNAME, REGISTRY_PASSWORD, GITHUB_OUTPUT.
set -euo pipefail

REGISTRY="docker.barkfluff.com"

case "$COMPONENT" in
  core|relay|dock) ;;
  *) echo "Неизвестный компонент $COMPONENT" >&2; exit 1 ;;
esac
REPO="lognara-$COMPONENT"

RESPONSE_FILE=$(mktemp)
HEADERS_FILE="${RESPONSE_FILE}.headers"
trap 'status=$?; rm -f "$RESPONSE_FILE" "$HEADERS_FILE"; exit "$status"' EXIT

FIRST_URL="https://$REGISTRY/v2/$REPO/tags/list"
TAGS_URL="$FIRST_URL"
SEEN_URLS=""
VERSIONS=""
while [ -n "$TAGS_URL" ]; do
  if [[ "$SEEN_URLS" == *"|$TAGS_URL|"* ]]; then
    echo "Повторная страница тегов $REPO" >&2
    exit 1
  fi
  SEEN_URLS="${SEEN_URLS}|${TAGS_URL}|"
  if ! HTTP_STATUS=$(curl -sS --connect-timeout 10 --max-time 30 --retry 3 --retry-delay 5 --retry-connrefused \
    -u "$REGISTRY_USERNAME:$REGISTRY_PASSWORD" -D "$HEADERS_FILE" -o "$RESPONSE_FILE" -w '%{http_code}' "$TAGS_URL"); then
    echo "Не удалось получить теги $REPO из реестра" >&2
    exit 1
  fi

  case "$HTTP_STATUS" in
    200)
      if ! jq -e '(.tags == null) or ((.tags | type) == "array" and all(.tags[]; type == "string"))' \
        "$RESPONSE_FILE" >/dev/null; then
        echo "Некорректный ответ реестра для $REPO" >&2
        exit 1
      fi
      PAGE_VERSIONS=$(jq -r '.tags[]? | select(test("^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"))' "$RESPONSE_FILE")
      VERSIONS="${VERSIONS}${VERSIONS:+$'\n'}${PAGE_VERSIONS}"
      ;;
    404)
      if [ "$TAGS_URL" != "$FIRST_URL" ] || ! jq -e '
        (.errors | type) == "array" and any(.errors[]; .code == "NAME_UNKNOWN")
      ' "$RESPONSE_FILE" >/dev/null; then
        echo "Репозиторий $REPO недоступен: HTTP 404" >&2
        exit 1
      fi
      break
      ;;
    *)
      echo "Не удалось получить теги $REPO: HTTP $HTTP_STATUS" >&2
      exit 1
      ;;
  esac

  NEXT_PAGE=$(awk 'tolower($1) == "link:" && /rel="?next"?/ {
    start = index($0, "<"); end = index($0, ">");
    print substr($0, start + 1, end - start - 1); exit
  }' "$HEADERS_FILE")
  case "$NEXT_PAGE" in
    "") break ;;
    "/v2/$REPO/tags/list?"*) TAGS_URL="https://$REGISTRY$NEXT_PAGE" ;;
    "$FIRST_URL?"*) TAGS_URL="$NEXT_PAGE" ;;
    *) echo "Некорректная ссылка следующей страницы тегов $REPO" >&2; exit 1 ;;
  esac
done

LAST=$(printf '%s\n' "$VERSIONS" | sed '/^$/d' | sort -Vr | sed -n '1p')
if [ -z "$LAST" ]; then
  VERSION="0.0.1"
else
  VERSION="${LAST%.*}.$(( ${LAST##*.} + 1 ))"
fi

echo "version=$VERSION" >> "$GITHUB_OUTPUT"
echo "Next $REPO release: $VERSION (previous: ${LAST:-none})"
