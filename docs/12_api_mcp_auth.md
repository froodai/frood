# Фаза 2. HTTP API, MCP и вход, v0.2

> Статус: **v0.1 одобрена 2026-10-17** (общий вердикт «апрув», ответы в §11). v0.2: вход только через внешних провайдеров. A2′ (REST) и A6 (транспорт MCP) ждут подтверждения после пояснений. Основание: D3 (HTTP API основной, MCP — адаптер), D9″ (пользователи без пространств и ролей), D10 (ядро на Rust), модель знаний v0.2 ([11_knowledge_model.md](11_knowledge_model.md)).
> Проверенные факты о клиентах — в §9 и [20_component_landscape.md](20_component_landscape.md).

## 1. Принципы

- **Один origin на инсталляцию**, например `https://mem.example.com`: `/v1/…` — API, `/mcp` — MCP, `/oauth/…` и `/.well-known/…` — вход, `/app` — веб-интерфейс.
- **OpenAPI 3.1 — источник истины.** MCP-инструменты, CLI и клиентские SDK — тонкие обёртки над теми же операциями.
- **Ядро без LLM.** API отвечает на структурированные запросы. Ответы на вопросы на естественном языке — опциональный модуль `/v1/ask`.
- **Эволюция только добавлениями** внутри `/v1`; несовместимое — `/v2` рядом. Для MCP то же правило (ChatGPT замораживает список инструментов, G4).
- **Соглашения:** ошибки — RFC 9457 (`application/problem+json`); команды идемпотентны по заголовку `Idempotency-Key`; конкурентные изменения — по версиям (`base` в Proposal, `If-Match`); пагинация курсорами; время — RFC 3339 UTC; у запросов на чтение есть `as_of` и `valid_at`.

## 2. Ресурсы

| Ресурс | Эндпоинты | Кто вызывает |
|---|---|---|
| Capture | `POST /v1/capture` — текст, аудио, файл или ссылка + подсказка «о чём». Возвращает `source_id` и `job_id`; извлечение идёт в фоне и создаёт Proposals | пользователь через любой клиент |
| Sources | `GET /v1/sources`, `GET /v1/sources/{id}`, `GET /v1/sources/{id}/content` | все с чтением |
| Observations | `POST /v1/observations`, `GET /v1/observations` | агенты, модули извлечения |
| Assertions | `GET /v1/assertions` (фильтры ниже), `GET /v1/assertions/{id}`, `GET /v1/assertions/{id}/history` | все с чтением |
| Entities | `GET /v1/entities`, `GET /v1/entities/{id}` (+ текущие утверждения), `GET /v1/entities/{id}/timeline` | все с чтением |
| Proposals | `POST /v1/proposals`, `GET /v1/proposals`, `GET /v1/proposals/{id}`, `PATCH /v1/proposals/{id}`, `POST /v1/proposals/{id}/rebase`, `POST /v1/proposals/{id}/verdicts` | агенты, пользователи |
| Inbox | `GET /v1/inbox` — открытые предложения и напоминания для текущего пользователя, по важности | пользователь |
| Search | `GET /v1/search?q=…` — модуль проекции поиска | все с чтением |
| Context | `GET /v1/context?entity=…\|topic=…` — сводка: текущее состояние, ответственные, решения, блокеры, изменения, источники | все с чтением |
| Ask (опционально) | `POST /v1/ask {question}` → ответ + ссылки на утверждения. Модуль с LLM | все с чтением |
| Journal | `GET /v1/journal?after=…`, `GET /v1/journal/stream` (SSE), `POST /v1/subscriptions` (вебхуки) | модули, интеграции |
| Users, tokens, clients | `GET /v1/me`, `GET/POST /v1/users`, `GET/POST/DELETE /v1/tokens`, `GET/DELETE /v1/clients` | пользователь, админ |
| Jobs | `GET /v1/jobs`, `GET /v1/jobs/{id}` (фаза 3) | пользователь, модули |

### Фильтры утверждений

`kind`, `entity`, `status`, `valid_at`, `as_of`, `text` и поля вида: `slot.<имя>.<оператор>=значение`, операторы `eq`, `gte`, `lt`, `in`.

«Какие сроки на этой неделе»:
```
GET /v1/assertions?kind=commitment&status=active&slot.due.gte=2026-10-19&slot.due.lt=2026-10-26
```

## 3. Вход

### 3.1 Кто выдаёт токены

**Инсталляция сама является OAuth 2.1 / OIDC Authorization Server** (тип модуля `identity`; реализация v1 встроена в ядро).

Почему не внешний IdP: AI-клиентам нужны регистрация клиентов через CIMD или DCR, refresh tokens, особые redirect URI (`https://claude.ai/api/mcp/auth_callback`, loopback для Claude Code). У большинства IdP этого нет или есть экспериментально (Keycloak — экспериментальный CIMD; Pocket ID — не подтверждено). Готовые AS с CIMD есть у облачных и TS-решений (Better Auth, Clerk, Ping), но не как лёгкий self-hosted сервис.

Как пользователь доказывает, кто он (v0.2, по ревью A3): **только через внешних провайдеров**, своих паролей и логинов нет. Наш AS делает федерацию: человек входит у провайдера, токены клиентам выдаёт наш AS.

| Провайдер | Заметки |
|---|---|
| Google | Бесплатный OAuth-клиент в Google Cloud; нам нужен только вход (ID token), доступ к API Google не запрашиваем |
| Apple | Нужен платный Apple Developer Program (~$99/год) для Services ID — минус для self-host ⚠️ |
| Microsoft (Entra ID) | Бесплатно; удобно для корпоративных аккаунтов |
| GitHub | Бесплатно |
| Корпоративный SSO по OIDC | Для рабочей инсталляции (QA2) |

Кто может войти: список разрешённых адресов и доменов в `frood.yaml` (`auth.allow: [me@gmail.com]`, `auth.allow_domains: [company.com]`). Ролей нет, поэтому «пригласить» = добавить адрес в список (через конфиг или API любым пользователем).

### 3.2 Клиенты и токены

| Клиент | Как получает доступ | Токен |
|---|---|---|
| AI-клиенты по MCP: ChatGPT, Claude, Claude Code, Codex, Cursor | Authorization code + PKCE S256; регистрация CIMD (предпочтительно), DCR — запасной вариант | короткий access (~1 ч) + refresh (`offline_access`), audience — `/mcp` |
| Веб-интерфейс | Тот же OAuth как first-party клиент | сессия |
| CLI | Device flow (RFC 8628) | access + refresh |
| Скрипты, iOS Shortcut, Telegram-бот | Personal access token с выбранными scopes и сроком | PAT (в БД только хэш) |
| Агенты, запускаемые системой | Ядро выпускает токен на запуск от имени пользователя: scopes = определение агента ∩ разрешения пользователя | run token, живёт на время запуска |
| Модули в отдельных процессах | Client credentials по манифесту модуля | service token, scopes из манифеста |

Требования к AS (из документации ChatGPT, Claude и спеки MCP):
- AS metadata: `client_id_metadata_document_supported: true`; `token_endpoint_auth_methods_supported` содержит `none` (условие Claude для CIMD) и `private_key_jwt` (ChatGPT для CIMD); `registration_endpoint` для DCR; `code_challenge_methods_supported: [S256]`.
- Protected resource metadata (RFC 9728) на `/.well-known/oauth-protected-resource`; `WWW-Authenticate` при 401.
- `iss` в ответе авторизации (RFC 9207); параметр `resource` (RFC 8707) учитывается в audience.
- Redirect URI: `https://claude.ai/api/mcp/auth_callback`; loopback `localhost` / `127.0.0.1` с любым портом (Claude Code); client_id ChatGPT — URL на chatgpt.com.
- Загрузка CIMD-документов с защитой от SSRF: только https, лимит размера, запрет приватных адресов, кэш.

### 3.3 Scopes — права токена, не роли

| Scope | Даёт |
|---|---|
| `knowledge:read` | читать утверждения, сущности, источники, историю, поиск, контекст, Inbox |
| `knowledge:capture` | отправлять заметки и файлы |
| `knowledge:propose` | создавать и править Proposals и Observations |
| `knowledge:decide` | ставить вердикты |
| `journal:read` | читать журнал и подписываться |
| `admin` | пользователи, токены, клиенты, настройки, redaction |

У пользователя все права. Токену, выданному клиенту, — только то, что пользователь разрешил на экране согласия. **AI-клиентам по умолчанию: read + capture + propose. `decide` — отдельная галочка, по умолчанию выключена**: иначе агент в чате может сам одобрить собственное предложение.

### 3.4 Что публично

- Публично по HTTPS: `/v1`, `/mcp`, `/oauth`, `/.well-known`, `/app`. Без этого облачные AI-клиенты не подключатся (G6).
- Только в приватной сети (Tailscale): админ-доступ к серверу, метрики, БД.
- Лимиты запросов на токен и IP; аудит входов и выданных токенов; отзыв клиента или токена в интерфейсе.

## 4. MCP-адаптер

- Streamable HTTP без сессий (спека 2026-07-28) + совместимость с клиентами 2025-11-25 (handshake `initialize`), пока клиенты не обновились. rmcp 3.x.
- Инструменты — тонкие обёртки над API; список фильтруется по scopes токена; порядок детерминированный; у читающих `readOnlyHint`.

| Инструмент | Scope | Для чего |
|---|---|---|
| `search_knowledge(query, kind?, entity?, status?, valid_at?, limit?)` | read | Поиск по смыслу и словам |
| `list_assertions(kind?, entity?, status?, due_from?, due_to?, owner?, valid_at?, as_of?)` | read | Структурные вопросы: «сроки на этой неделе», «за что отвечает Оля» |
| `get_context(topic? \| entity_id?, as_of?)` | read | Сводка по проекту или человеку |
| `get_entity(entity_id)` | read | Сущность и её текущие утверждения |
| `get_history(assertion_id? \| entity_id?)` | read | Как менялось |
| `capture_note(text, about?)` | capture | «Запомни: …» |
| `propose_change(operations, rationale, evidence?)` | propose | Структурное предложение изменения |
| `list_inbox()` | read | Что ждёт решения |
| `get_proposal(proposal_id)` | read | Детали предложения |
| `submit_verdict(proposal_id, verdict, alternative?, edits?, reason?)` | decide | Вердикт; виден только с этим scope |

- **MCP Apps:** `get_proposal` и `list_inbox` отдают `_meta.ui.resourceUri` с карточкой (старое и новое, доказательства, кнопки). Где UI не поддерживается — текст. Кнопки вызывают `submit_verdict` там, где клиент умеет вызывать инструменты из UI; иначе ссылка в веб-Inbox.
- **Долгие операции** (`capture_note`, `investigate`) сразу возвращают id задачи или предложения; клиент перечитывает через `get_proposal` / `list_inbox`. Расширение Tasks — позже.
- **Инструкции сервера для модели:** сначала искать, цитировать id утверждений и источников, новые факты — через `capture_note` / `propose_change`, ничего не выдавать за известное без источника.

## 5. Журнал для модулей и интеграций

- `GET /v1/journal?after=<position>` — страницы событий; `GET /v1/journal/stream` — SSE; `POST /v1/subscriptions` — вебхуки.
- Конверт CloudEvents + `position`, `actor`; вебхуки подписаны HMAC.
- Доставка «хотя бы раз»; потребители идемпотентны по `id` события и хранят свой checkpoint.

## 6. Матрица клиентов

| Клиент | Чтение | Ввод и предложения | Вердикт | Карточки | Примечание |
|---|---|---|---|---|---|
| Claude web / desktop | MCP | MCP | MCP, если разрешено | MCP Apps | на Free один коннектор |
| Claude mobile | MCP | MCP | MCP, если разрешено | ⚠️ | коннектор добавляется в вебе |
| ChatGPT web, Business / Enterprise | MCP | MCP | MCP, если разрешено | MCP Apps без вызова инструментов из UI ⚠️ | в Enterprise подключение может требовать админа воркспейса |
| ChatGPT Pro | MCP | — | — | — | только чтение |
| ChatGPT mobile | ⚠️ | — | — | — | подключение только в вебе; использование с телефона не подтверждено |
| Claude Code, Codex, Cursor | MCP или HTTP | да | если разрешено | — | |
| OpenClaw, Hermes | MCP | да | если разрешено | — | готовые проактивные каналы |
| Telegram-бот (модуль) | HTTP + PAT | голос, текст | кнопки | — | |
| iOS Shortcut | — | HTTP + PAT | — | — | быстрый ввод одной кнопкой |
| Веб-интерфейс | HTTP | да | да | — | полный доступ |

## 7. Примеры

1. **«Какие сроки на этой неделе?» в Claude mobile** → `list_assertions(kind=commitment, due_from=2026-10-19, due_to=2026-10-25)` → `GET /v1/assertions?…` → ответ со ссылками на утверждения и источники.
2. **Быстрый ввод с iPhone** → Shortcut «Запомнить» → диктовка → `POST /v1/capture` с PAT (scope `capture`) → источник и задача → извлечение → свой факт без конфликта принят политикой.
3. **Подключение ChatGPT Enterprise** → URL `https://mem.example.com/mcp` → ChatGPT читает protected resource metadata и AS metadata → CIMD (client_id — URL на chatgpt.com) → вход через корпоративный SSO → согласие: read, capture, propose; decide выключен → токены.

## 8. Решения на ревью

| ID | Решение |
|---|---|
| A1 | Один origin на инсталляцию; OpenAPI 3.1 — источник истины; `/v1` меняется только добавлениями; problem+json, Idempotency-Key, курсоры, версии |
| A2 | Структурированные запросы (вид, поля, время) — в API; ответы на естественном языке — опциональный модуль `/v1/ask` |
| A3 | Инсталляция — сама себе OAuth/OIDC Authorization Server (модуль identity, v1 встроен); вход только через внешних провайдеров (Google, Apple, Microsoft, GitHub, корпоративный SSO) |
| A4 | OAuth + PKCE с CIMD/DCR для AI-клиентов; device flow для CLI; PAT для скриптов и ботов; токен на запуск агента от имени пользователя; service-токены модулей |
| A5 | Scopes вместо ролей; AI-клиентам по умолчанию read + capture + propose, decide — явная галочка |
| A6 | MCP: Streamable HTTP без сессий + совместимость с 2025-11-25; 10 инструментов-обёрток; список по scopes; изменения только добавлениями |
| A7 | MCP Apps-карточки с текстовым запасным вариантом; долгие операции — id и перечитывание, без Tasks в v1 |
| A8 | Журнал для модулей: страницы + SSE + подписанные вебхуки, доставка «хотя бы раз» |

## 9. Проверенные факты (2026-10-17)

- ChatGPT: поддерживает CIMD (по умолчанию, если сервер объявил) и DCR, PKCE, заранее заданных клиентов; для CIMD — `none` или `private_key_jwt` ✅ [OpenAI Apps SDK: Authentication](https://developers.openai.com/apps-sdk/build/auth). client_id — URL на chatgpt.com, передаёт `resource` ⚠️ [Zuplo](https://zuplo.com/learn/mcp/compatibility/clients/chatgpt-connectors).
- Claude: DCR и CIMD из коробки; CIMD выбирается, если AS объявляет `client_id_metadata_document_supported: true` и `none`; redirect `https://claude.ai/api/mcp/auth_callback`; Claude Code — loopback с любым портом; PKCE S256 всегда ✅ [Claude: connector authentication](https://claude.com/docs/connectors/building/authentication).
- Self-hosted AS с CIMD: Better Auth (плагин), Clerk (beta), Ping, ServiceNow ⚠️ [Better Auth CIMD](https://better-auth.com/docs/plugins/cimd); Pocket ID — поддержка DCR не подтверждена ⚠️; Ory Hydra — не нашёл данных ⚠️.
- Rust: паттерн resource server с валидацией JWT и scopes есть в tower-mcp ⚠️ [tower-mcp oauth](https://docs.rs/crate/tower-mcp/0.20.1/source/src/oauth/mod.rs); `auth` в rmcp — проверить, серверная это часть или клиентская ⚠️.

## 10. Вопросы

- Сколько людей будет в рабочей инсталляции: только автор или коллеги тоже?
- Вход в рабочей инсталляции: хватит passkey или нужен корпоративный SSO?
- Разрешать ли AI-клиентам вердикты по умолчанию? (Предложено: нет, галочка при подключении.)
- Есть ли домен для VPS, и ок ли, что API и MCP будут доступны из интернета (под OAuth)?

## 11. Ответы на ревью v0.1 (2026-10-17)

Общий вердикт: **апрув**.

| Пункт | Отметка | Комментарий автора | Что изменено |
|---|---|---|---|
| A1, A4, A5, A8 | принять | — | — |
| A2 | обсудить | REST, GraphQL — что брать? | Пояснение ниже → A2′ (REST + OpenAPI) на подтверждение |
| A3 | принять | Только внешние сервисы (Google, Apple); свой email и пароль не нужны | §3.1: вход только через внешних провайдеров, список разрешённых адресов; passkey убран |
| A6 | без отметки | Нет вебсокетов в MCP? | Пояснение ниже, на подтверждение |
| A7 | обсудить | Тут надо аккуратно, но в целом ок | Принято с оговорками ниже |
| QA1 | — | Пока только автор, со временем человека 3 | Модель без пространств подходит; вход по списку адресов |
| QA2 | — | SSO | Рабочая инсталляция — корпоративный SSO по OIDC |
| QA3 | — | Вердикты AI-клиентам по умолчанию не нужны | Как предложено |
| QA4 | — | Домен и публичный доступ ок | — |

### A2′: почему REST, а не GraphQL

| Стиль | Плюсы | Минусы для нас |
|---|---|---|
| **REST + OpenAPI** | Понимают все: curl, iOS Shortcuts, Telegram-боты, ChatGPT Actions, генераторы клиентов; кэширование, вебхуки, SSE ложатся естественно; в Rust зрелые axum + utoipa | Сложные выборки делаются отдельными эндпоинтами |
| GraphQL | Клиент сам выбирает поля и вложенность одним запросом; удобно сложному UI | Сложнее кэш и права на уровне полей, проблема N+1; AI-клиенты и Shortcuts работают с ним хуже; второй язык запросов рядом с MCP |
| gRPC | Быстро между сервисами | Неудобно из браузера, Shortcuts и AI-клиентов |
| JSON-RPC | Это уже MCP | Второй такой же не нужен |

Главное преимущество GraphQL — собрать вложенные данные одним запросом — у нас закрывают `/v1/context` и `/v1/entities/{id}` (сущность с текущими утверждениями). Если веб-интерфейсу когда-нибудь понадобится GraphQL, его можно добавить модулем-фасадом только для чтения.

**A2′:** HTTP API в стиле REST с описанием OpenAPI 3.1; структурные запросы в API; ответы на естественном языке — модуль `/v1/ask`.

### A6: есть ли у MCP вебсокеты

- В спеке MCP два транспорта: stdio (локально) и Streamable HTTP (удалённо). WebSocket-транспорт предложен (SEP-1288), но в спеку не вошёл ⚠️. ChatGPT и Claude подключаются к удалённым серверам по Streamable HTTP.
- В версии 2026-07-28 сервер может слать уведомления клиенту через долгий поток `subscriptions/listen`, но это уведомления об изменении списка инструментов и ресурсов, а не сообщения пользователю.
- Для нас вебсокеты не нужны: проактивные сообщения идут через каналы уведомлений (фаза 5), живые обновления веб-интерфейса — через SSE из `/v1/journal/stream`.

**A6 без изменений:** MCP по Streamable HTTP без сессий + совместимость с 2025-11-25.

### A7: оговорки «аккуратно»

- Текст — основной ответ инструмента; карточка MCP Apps — только дополнение.
- Кнопка вердикта в карточке показывается, только если у токена есть `decide` и клиент умеет вызывать инструменты из UI; иначе ссылка в веб-Inbox.
- В карточке нет секретов и токенов; данные только те, что уже вернул инструмент; строгий CSP.
- Поддержка проверяется по каждому клиенту отдельно (у Claude нужна подпись домена, у ChatGPT нет вызова инструментов из UI ⚠️).

## 12. Добавлено фазой 3 (только добавления, A1)

| Ресурс | Эндпоинты |
|---|---|
| runs | `GET /v1/runs`, `GET /v1/runs/{id}`, `POST /v1/runs` (ручной запуск), `POST /v1/runs/{id}/messages` (управление на ходу), `POST /v1/runs/{id}/cancel` |
| questions | `GET /v1/questions`, `POST /v1/questions/{id}/answer` |
| config | `GET /v1/config/revisions`, `POST /v1/config` (push-источник), `POST /v1/config/rollback` |

MCP: для пользователей — `list_questions`, `answer_question`; для токена запуска — `ask_user`, `report_progress`.
