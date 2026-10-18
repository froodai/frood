# Журнал реализации

> Что сделано по плану [17_mvp_plan.md](17_mvp_plan.md), как проверено, чем отличается от плана и что ждёт решения автора.

## Ночь 17→18 октября 2026

### M0. Скелет — готов

- Монорепозиторий: `core/` (Rust workspace), `contracts/` (+ полный текст Apache-2.0), `modules/`, `bundles/standard/`, `deploy/`, `docs/`, `.github/workflows/ci.yml`, корневые `README.md` и `LICENSE`.
- Крейты: `frood-config` (модель `frood.yaml`, `${secret:…}` / `${env:…:-default}`, проверка, JSON Schema, шаблон), `frood-store` (подключение к Postgres, миграции, ревизии конфига), `frood-api` (axum), `frood-cli` (бинарник `frood`).
- Команды: `frood init | plan | apply [--render-only] | serve | status | config schema`, скрытая `probe` для health-check контейнера.
- `apply` рендерит `.frood/deploy/compose.yaml` (Postgres 18, при `deploy.server: container` — ещё сервер), поднимает его, мигрирует БД и записывает ревизию конфига (сырой текст с неразрешёнными `${secret:…}`, секретов в БД нет).
- Dockerfile (Rust 1.99 / Debian trixie), CI: fmt, clippy `-D warnings`, тесты с Postgres-сервисом, сборка образа.

**Проверено:** юнит-тесты; вживую на локальном Postgres — `init`, `plan`, `apply --render-only`, `serve` (миграции, `/healthz`, `/readyz`, `/v1/version`), `status`.
**Не проверено:** `apply` с настоящим `docker compose up` и сборка образа — Docker Desktop сломан (см. ниже).

### M1. Ledger + API — готов

- Крейт `frood-ledger`: источники и доказательства, наблюдения, сущности (создание, слияние, разделение), утверждения как неизменяемые bitemporal-строки (`valid_*`, `recorded_*`), предложения с альтернативами, вердикты, флаги `contested` / `needs_verification`, связи, журнал.
- Инварианты в БД: текущее знание об утверждении не пересекается по valid-времени (exclusion constraint на `btree_gist`); журнал пишется под транзакционной advisory-блокировкой, поэтому позиции видны строго по порядку.
- Вердикт применяет альтернативу атомарно, предварительно заблокировав затронутые утверждения и проверив базу; устаревшая база → предложение `stale`, ошибка 409.
- Политики (M8, M11): свои новые факты пользователя без конфликта и слияния сущностей с уверенностью ≥ 0.9 принимаются автоматически, вердикт подписан `policy:<имя>`; настраивается секцией `policies` в `frood.yaml`.
- API `/v1`: `sources`, `evidence`, `observations`, `assertions` (фильтры `kind`, `entity`, `status`, `text`, `valid_at`, `as_of`, `slot.<имя>.<eq|gte|lt|in>`, курсор), `assertions/{id}` (+ `?as_of=&valid_at=`), `assertions/{id}/history`, `entities`, `proposals` (+ `Idempotency-Key`), `proposals/{id}/verdicts`, `journal`. Ошибки — RFC 9457.

**Проверено:** 35 тестов (8 интеграционных тестов Ledger, сквозной тест API, юнит-тесты), clippy `-D warnings`, живой прогон через curl: голосовая заметка → своё утверждение принято политикой → конфликт от агента (флаг `contested`) → вердикт → на 1 октября срок 14.11 (v2, superseded), на 1 ноября 28.11 (v3, active).

### Отличия от плана

| Где | Что | Почему |
|---|---|---|
| Крейты | Добавлен `frood-store` (БД, миграции, ревизии конфига); Ledger — `frood-ledger` | Все миграции ядра в одной последовательности; конфиг-крейт не тянет sqlx |
| Вход в M1 | Запись требует заголовок `Frood-Actor: user:<имя>`; сервер по умолчанию слушает только 127.0.0.1 | Настоящий вход — M2; до него API не должен смотреть наружу |
| Утверждение при замене | Замена создаёт две строки: старое содержание с концом действия (`superseded`) и новое (`active`); старая строка получает `recorded_to` | Корректная bitemporal-модель: и «что действовало 1 октября», и «что мы знали 10 октября» отвечаются запросами |

### Изменения окружения

| Что | Зачем | Как откатить |
|---|---|---|
| `rustup update stable` → Rust 1.99.1 | С разрешения автора; sqlx 0.9 требует 1.94+ | — |
| `brew install postgresql@18` (keg-only, не в PATH, служба не запущена) | Docker Desktop не создаёт контейнеры; нужен Postgres для тестов | `brew uninstall postgresql@18` |
| Временный кластер Postgres в scratchpad, порт 55433 | Интеграционные тесты | Останавливается в конце работы |
| `git init` в корне проекта, без коммитов | Подготовка репозитория | `rm -rf .git` |

### Docker Desktop сломан

`docker run` падает: `error creating temporary lease: write /var/lib/desktop-containerd/daemon/io.containerd.metadata.v1.bolt/meta.db: input/output error`. Работающие 5 недель контейнеры `jcp-platform-*` при этом живы. Лечится перезапуском Docker Desktop, но он остановит эти контейнеры, поэтому без автора не трогал.

### Ждёт решения автора

1. Имя организации на GitHub — свободны `froodai`, `frood-ai`, `froodlabs`, `heyfrood` и др.; в путях образов пока `ghcr.io/froodai/…`. Организацию создаёт автор.
2. Перезапуск Docker Desktop (остановит `jcp-platform`), чтобы проверить `apply` и сборку образа.
3. Полный текст AGPL-3.0 в `core/LICENSE` — скачать с gnu.org (сейчас там заглушка со ссылкой).
4. Коммит: работа лежит незакоммиченной; делать ли первый коммит.
5. Для живой проверки M2 с ChatGPT и Claude: OAuth-клиент Google (или корпоративный OIDC), домен и публичный HTTPS на VPS.

### Как запустить тесты локально

```bash
cd core
FROOD_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:55433/postgres cargo test
```

Без `FROOD_TEST_DATABASE_URL` интеграционные тесты пропускаются сами.

### M2. Вход + MCP — начат, остановлен из-за лимита использования

Готово (компилируется, не подключено к серверу и не покрыто интеграционными тестами):
- `frood.yaml`: типизированная секция `auth` (allow, allow_domains, providers `oidc` / `dev`, сроки токенов) с проверками.
- Миграция `0003_auth.sql`: users, oauth_clients, auth_requests, auth_codes, sessions, tokens (секреты только хэшами).
- Крейт `frood-auth`: секреты и PKCE (`secret.rs`), токены — проверка Bearer с аудиторией, выдача пары, ротация refresh с отзывом семьи при повторе, личные токены (`tokens.rs`), allowlist и пользователи (`users.rs`), клиенты — CIMD с защитой от SSRF и кэшем, DCR, правила redirect URI (`clients.rs`), OIDC — discovery, PKCE, проверка ID-токена по JWKS (`oidc.rs`), HTML-страницы входа и согласия (`pages.rs`).
- Крейт `frood-mcp`: заготовка (только зависимость rmcp 3.5).

Осталось:
1. `frood-auth/src/web.rs`: обработчики `/.well-known/oauth-authorization-server`, `/.well-known/oauth-protected-resource[/mcp|/v1]`, `/oauth/authorize`, `/oauth/login[/{provider}]`, `/oauth/callback/{provider}`, `/oauth/consent`, `/oauth/token`, `/oauth/register`, `/oauth/revoke`; сессионная cookie; проверка Origin.
2. `frood-api`: заменить заголовок `Frood-Actor` на Bearer-аутентификацию с проверкой scopes, 401/403 с `WWW-Authenticate`; `/v1/tokens`.
3. `frood-mcp`: Streamable HTTP через `StreamableHttpService` + `NeverSessionManager`, принципал из `Parts.extensions`, инструменты по scopes (`search_knowledge`, `list_assertions`, `get_context`, `capture_note`, `propose_change`, `list_inbox`, `get_proposal`, `submit_verdict`), монтирование в `/mcp`.
4. CLI: `frood token create`.
5. Тесты: e2e-поток OAuth с моковым OIDC-провайдером (тестовый RSA-ключ сгенерирован в scratchpad, n посчитан), DCR, refresh, MCP-вызовы; clippy `-D warnings` (сейчас в `frood-auth` будут предупреждения о неиспользуемом коде, пока нет `web.rs`).

Состояние окружения на момент остановки: тестовый Postgres остановлен; запустить снова:
`/opt/homebrew/opt/postgresql@18/bin/pg_ctl -D <scratchpad>/pgdata -o "-p 55433 -c listen_addresses=127.0.0.1 -c unix_socket_directories=''" -l <scratchpad>/pg.log start`.
