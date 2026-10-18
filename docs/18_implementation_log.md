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
**Проверено 18.10 после перезапуска Docker:** полный `init` → `apply` с образом, собранным из исходников, → миграции → `status` (ready). Нашлись и исправлены две ошибки: в образе не было корневых сертификатов (HTTP-клиент на rustls не создавался, health-check падал) и `sqlx::migrate!` не пересобирался при новой миграции (добавлен `build.rs`).

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
| Автор действий AI-клиента (R1′) | Токен OAuth-клиента (Claude, ChatGPT, Claude Code) действует как сам человек, клиент записывается для аудита: `user:<id>/via/<клиент>`. Агенты, работающие сами по расписанию (M4), получат run-токены и будут писать как `agent:<джоба>` от имени человека — их предложения идут на ревью | Решение автора в раунде 10: агент, которому человек сам сказал что-то сделать, и агент в облаке на кроне — разные вещи. Новые факты без конфликта принимаются политикой «свои факты»; изменения и конфликты всё равно ждут вердикта, поэтому `decide` по-прежнему нужен |
| `/v1/tokens` (R2, принято) | Создание, список и отзыв личных токенов требуют scope `admin`; первый токен выпускает оператор командой `frood token create` | Токен AI-клиента не должен уметь выпускать себе долгоживущие токены |
| `capture_note` | Сохраняет заметку как источник и возвращает `evidence_id`; извлечения пока нет (M3), инструмент так и отвечает модели | Честный ответ вместо обещания, что факт «запомнен» |
| `propose_change` | Добавлен параметр `source_text`: слова, на которых основано изменение; сохраняются источником и становятся доказательством | Утверждению нужно доказательство, а у реплики в чате его нет |
| `/v1/clients`, `/v1/users` | Не сделаны | Это админка, M6 |
| Утверждение при замене | Замена создаёт две строки: старое содержание с концом действия (`superseded`) и новое (`active`); старая строка получает `recorded_to` | Корректная bitemporal-модель: и «что действовало 1 октября», и «что мы знали 10 октября» отвечаются запросами |

### Изменения окружения

| Что | Зачем | Как откатить |
|---|---|---|
| `rustup update stable` → Rust 1.99.1 | С разрешения автора; sqlx 0.9 требует 1.94+ | — |
| `brew install postgresql@18` (keg-only, не в PATH, служба не запущена) | Docker Desktop не создаёт контейнеры; нужен Postgres для тестов | `brew uninstall postgresql@18` |
| Контейнер `frood-testdb` (postgres:18, порт 127.0.0.1:55433, trust) | Интеграционные тесты | `docker rm -f frood-testdb` |
| `git init` в корне проекта; `user.name` / `user.email` автора в локальном конфиге репозитория | Коммиты от имени автора | — |
| Приватный репозиторий `froodai/frood`, remote `origin` | Ответ автора QR1 | — |
| Локальный Frood в `~/frood-local` (Docker Compose-проект `frood-local`: Postgres на 127.0.0.1:54320, сервер на 127.0.0.1:8080, вход `dev` для адреса автора) | Ответ автора QR2: попробовать с Claude Code | `docker compose -p frood-local down -v`, удалить `~/frood-local` |
| MCP-сервер `frood` в пользовательском конфиге Claude Code (`~/.claude.json`) | То же | `claude mcp remove frood -s user` |

### Docker Desktop

Был сломан (ошибка containerd); автор перезапустил 18.10, всё работает. Контейнеры `jcp-platform-*` после перезапуска остановлены, я их не запускал.

### Решено 18.10

Организация `froodai` создана; Docker перезапущен; полный текст AGPL-3.0 в `core/LICENSE`; коммиты идут от имени автора.

### Как запустить тесты локально

```bash
cd core
FROOD_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:55433/postgres cargo test
```

Без `FROOD_TEST_DATABASE_URL` интеграционные тесты пропускаются сами.

### M2. Вход + MCP — готов (без проверки на настоящих ChatGPT и Claude)

- **Сервер авторизации** (`frood-auth`): метаданные AS (RFC 8414, с `client_id_metadata_document_supported`, `authorization_response_iss_parameter_supported`) и защищённых ресурсов `/v1` и `/mcp` (RFC 9728); `/oauth/authorize` → выбор провайдера → вход → согласие → код; `/oauth/token` (код + PKCE S256, refresh с ротацией; повтор refresh или кода отзывает всю семью токенов); `/oauth/register` (DCR); `/oauth/revoke` (RFC 7009). Клиенты — CIMD (документ по `https://` client_id, защита от SSRF, кэш) или DCR.
- **Вход людей** — только через внешнего OIDC-провайдера (Google, Entra, корпоративный SSO): discovery, PKCE, проверка ID-токена по JWKS, nonce, `email_verified`; пускаются адреса из `auth.allow` / `auth.allow_domains`. Провайдер `dev` (ввести email) разрешён только на loopback без `public_url`. Сессия — cookie `frood_session` (HttpOnly, SameSite=Lax, Path=/oauth, Secure при https); POST-формы проверяют Origin.
- **Согласие:** по умолчанию read + capture + propose; `decide` никогда не отмечен заранее; `admin` предлагается, только если клиент его запросил.
- **API:** каждый маршрут `/v1` требует Bearer-токен с нужным scope; 401 и 403 несут `WWW-Authenticate` со ссылкой на метаданные ресурса. Токен, выданный для `/mcp`, на `/v1` не принимается. Новые маршруты: `/v1/me`, `/v1/tokens` (личные токены). Заголовок `Frood-Actor` удалён.
- **MCP** (`frood-mcp`, rmcp 3.5): Streamable HTTP без сессий, JSON-ответы, защита от DNS-rebinding по хосту issuer; 10 инструментов из docs/12 §4, список фильтруется по scopes токена (без `decide` нет `submit_verdict`); инструкции сервера для модели.
- **CLI:** `frood token create | list | revoke` — личные токены для скриптов и ботов (первый токен инсталляции).

**Проверено:**
- 48 тестов: сквозной OAuth-поток с моковым OIDC-провайдером (Ed25519-ключ генерируется в тесте) — discovery по 401, DCR, отказ на чужом redirect URI, вход, согласие, неверный PKCE, обмен кода, аудитория токена, MCP `initialize` / `tools/list` / `capture_note` / `propose_change` / `list_inbox` / отказ `submit_verdict`, ротация refresh, отзыв семьи при повторе, одноразовость кода; отказ человеку вне allowlist; токены и scopes на `/v1`. clippy `-D warnings`, rustfmt.
- Вживую на собранном бинарнике: 401 с `WWW-Authenticate` на `/mcp`, метаданные, `frood token create`, MCP через curl, `/v1/me`; `/oauth/authorize` с настоящим client_id Claude (`https://claude.ai/oauth/mcp-oauth-client-metadata`) — документ скачан с claude.ai, redirect `https://claude.ai/api/mcp/auth_callback` принят.

**Не проверено:** настоящие ChatGPT и Claude (нужны публичный HTTPS и OIDC-клиент, см. ниже) и Claude Code (можно локально, но это меняет конфиг Claude Code у автора — без спроса не делал).

### Что нужно для проверки с ChatGPT и Claude (пункт 5)

Claude и ChatGPT подключаются к MCP-серверу **из своего облака**, а не с компьютера автора, и человек при подключении входит в Frood через браузер. Поэтому:

1. **Публичный адрес по HTTPS** — домен (например, `mem.example.com`), направленный на VPS, где запущен Frood за обратным прокси с сертификатом (Caddy получает его сам). Адрес `127.0.0.1` из облака Anthropic или OpenAI не виден.
2. **OAuth-клиент у провайдера входа**, чтобы Frood мог спросить у Google (или корпоративного SSO), кто входит: в Google Cloud Console → APIs & Services → Credentials → OAuth client ID (Web application), redirect URI `https://<домен>/oauth/callback/google`. Получаются client_id и client_secret, они идут в `frood.yaml` (`auth.providers`) и в `.frood/secrets.env`.

Claude Code и другие локальные клиенты ничего из этого не требуют: Frood на `127.0.0.1` с провайдером `dev` или личный токен.

