# Ландшафт готовых компонентов (ресерч от 2026-10-17)

> Что существует под каждый кусок системы, какие у него API и ограничения. Метки: ✅ подтверждено первичным источником; ⚠️ только вторичные источники или не до конца проверено, перепроверить на этапе компонента.
> Кандидаты, отобранные в дизайн, есть в [10_high_level_architecture.md](10_high_level_architecture.md). Ловушки собраны в [30_constraints_and_gotchas.md](30_constraints_and_gotchas.md).

## 1. Каноническое хранилище / Ledger

| Вариант | Что даёт | Минусы | Вывод |
|---|---|---|---|
| **PostgreSQL** (свой слой событий, assertions и proposals) | Транзакции, JSONB, pgvector, FTS, outbox, одна СУБД на всё; в PG 18 есть темпоральные ограничения `WITHOUT OVERLAPS` ⚠️ | Bitemporal-модель и PR-workflow пишем сами | **v1** |
| **Doltgres** | Postgres-совместимая СУБД с git-семантикой: branch / merge / diff данных и схемы. 1.0 вышел 2026-08-06, хранилище стабильно в 1.x ✅ | ~2.7× медленнее Postgres ⚠️; в roadmap ещё vector indexes и RLS ✅; маленькая экосистема | Интересно для «proposal = ветка», но только эксперимент |
| XTDB v2 | Нативно bitemporal SQL | Нишевый, отдельный движок | Только референс модели |
| Модель Wikidata (statement + qualifiers + references + rank) | Готовая модель «утверждение + уточнения + источники + ранг (preferred / normal / deprecated)» | Это модель, не продукт | Взять как референс для схемы assertion |

Источники: [Doltgres 1.0](https://dolthub.com/blog/2026-08-06-doltgres-1-0/), [Doltgres extensions update](https://dolthub.com/blog/2026-09-03-doltgres-extensions-update/).

## 2. Memory-движки (кандидаты в проекции, не в канон)

| Проект | Суть | Применимость |
|---|---|---|
| **Graphiti** (Zep) | Temporal knowledge graph, эпизоды, извлечение LLM. MCP Server 1.0: YAML-конфиг, FalkorDB по умолчанию (Neo4j тоже), Streamable HTTP, 9 предзаданных entity types ✅. Есть `add_triplet(source, edge, target)` с дедупликацией ✅; нужны ли при этом LLM-вызовы и можно ли задать `valid_at`, в доке не сказано ⚠️ | Проекция графа: подаём уже **одобренные** утверждения через `add_triplet`, свою экстракцию не используем, если она меняет граф без ревью |
| **Mem0 / OpenMemory** | Быстрая память с MCP | Плохо подходит под жизненный цикл договорённостей; заявленные бенчмарки спорны (49% vs 94.8% на LongMemEval в разных источниках) ⚠️ |
| **Cognee** | Graph-native, локальный стек SQLite + LanceDB + Kuzu, open core ⚠️ | Альтернатива Graphiti для проекции |
| **Letta** | Уровни памяти core / recall / archival, агент сам управляет контекстом; sleep-time агенты | Референс для фоновой реорганизации |
| **Hindsight** (vectorize.io) | MIT, хорошие заявленные результаты на BEAM / LongMemEval (самоотчёт) ⚠️ | Посмотреть как проекцию |
| Basic Memory | Markdown-файлы + индекс + MCP | Референс для человекочитаемого экспорта |

Общий вывод из обзоров: LongMemEval почти насыщен и уже плохо различает системы; оценивать надо на своих данных ⚠️.
Источники: [Graphiti MCP 1.0](https://blog.getzep.com/graphiti-hits-20k-stars-mcp-server-1-0/), [Graphiti fact triples](https://help.getzep.com/graphiti/working-with-data/adding-fact-triples), [обзор Hindsight 2026](https://hindsight.vectorize.io/blog/2026/08/11/open-source-agent-memory-systems), [обзор Cognee](https://www.cognee.ai/open-source-memory-frameworks-llm-agents).

## 3. Work engine (события, cron, durable tasks, HITL)

| Вариант | Модель | Плюсы | Минусы |
|---|---|---|---|
| **DBOS Transact** | Библиотека (Python / TS / Java), чекпоинты в системной БД внутри Postgres ✅ | Без отдельного сервера; cron-декоратор, с апреля 2026 часовые пояса и **backfill пропущенных cron-запусков** ✅; durable sleep; `send`/`recv` с таймаутом для ожидания решения человека днями ✅ | UI (Conductor) **проприетарный**, self-host в проде только по лицензии ✅, так что Activity UI делаем сами; Transact заявлен как MIT ⚠️ |
| **Hatchet** | Отдельный сервис (Go) + Postgres, SDK | MIT ✅, свой UI, очереди, rate limits, OTel; последняя версия v0.100.2 (июль 2026) ✅ | Ещё один сервис; v1-стабильности нет |
| **Absurd** (A. Ronacher) | Один SQL-файл с хранимыми процедурами, только Postgres ✅ | Минимализм; 5 месяцев в проде у автора ✅ | Нет партиционирования и push-планирования ✅; молодой |
| Temporal | Отдельный кластер | Зрелый | Тяжёл для VPS |
| Windmill / n8n / Dify | Платформы с UI | Быстро получить UI | Логика уезжает в платформу; AGPL / fair-code |

**Решение v1:** DBOS за контрактом `work.*`; Hatchet — запасной provider того же контракта.
Источники: [Why DBOS](https://docs.dbos.dev/why-dbos), [DBOS апрель 2026](https://dbos.dev/blog/dbos-new-features-april-2026), [Self-hosting Conductor](https://docs.dbos.dev/production/hosting-conductor), [Hatchet](https://pkg.go.dev/github.com/hatchet-dev/hatchet@v0.100.2), [Absurd: 5 месяцев](https://tproger.ru/translations/absurd-pyat-mesyacev-durable-execution-na-postgres-otchyot-armin).

## 4. Агентные рантаймы и определения агентов

### Рантаймы (кто исполняет)

| Вариант | Тип | Заметки |
|---|---|---|
| Claude Agent SDK | Библиотека Python / TS | Headless-агент с инструментами и MCP |
| Codex SDK / `codex exec` | Библиотека TS / CLI | Headless Codex |
| **OpenCode `serve`** | Self-hosted HTTP-сервер: OpenAPI 3.1, SSE-события, JS/Go SDK, Basic Auth ✅ | Model-agnostic (в т.ч. локальные модели) ⚠️ — сильный кандидат на «свой» рантайм без вендора |
| **ACP (Agent Client Protocol)** | Протокол управления агентами (Zed) | Gemini CLI нативно; Claude Code и Codex через адаптеры `claude-code-acp` / `codex-acp` ⚠️. Возможная единая абстракция `agents.execute` |
| **Claude Managed Agents** | Облачный API (beta, заголовок `managed-agents-2026-04-01`): agent / environment / session / events; сэндбокс у Anthropic **или self-hosted** (E2B, Daytona) ✅ | Состояние сессий хранится на стороне Anthropic, не подпадает под ZDR ✅. Вариант для «облачного агента по событию» |
| Factory Droid, Cursor background agents, Devin и др. | Облачные | Адаптеры по мере нужды |

### Определения агентов (что исполнять)

- **Agent Skills** (`SKILL.md`, agentskills.io): открытый стандарт с декабря 2025, поддерживается Claude Code, Codex, Cursor и 30–40+ инструментами ⚠️. Экспериментальное поле `allowed-tools` поддерживают не все ⚠️.
  → Формат инструкций для наших агентов: переносим между рантаймами, версионируется в git.
- Своё поверх Skills: манифест агента (триггеры, разрешённые MCP-инструменты, scope токена, контракт результата «список Proposals», бюджет).

Источники: [Zed external agents](https://zed.dev/docs/ai/external-agents), [ACP в AI SDK](https://ai-sdk.dev/providers/community-providers/acp), [OpenCode server](https://opencode.ai/docs/de/server), [Managed Agents overview](https://platform.claude.com/docs/en/managed-agents/overview), [Cursor: Agent Skills](https://www.cursor.com/docs/context/skills), [Agent Skills across tools](https://codex.danielvaughan.com/2026/05/05/agent-skills-open-standard-portable-skills-codex-cli-cross-agent/).

## 5. Готовые персональные ассистенты (поверхности или рантаймы, не ядро)

| Проект | Что это | Как использовать |
|---|---|---|
| **OpenClaw** | Self-hosted ассистент: Gateway-процесс, каналы (WhatsApp / Telegram / …), heartbeat (по умолчанию раз в 30 мин), cron, память в Markdown/YAML + vector/FTS ⚠️. У практиков плоская память «разваливается» через ~10 дней ⚠️ | Подключить как клиент к нашему Memory MCP; его собственная память не канон |
| **Hermes Agent** (Nous Research) | MIT, gateway для Telegram / Discord / Slack / WhatsApp / Signal, cron, скиллы в формате agentskills.io, память в `~/.hermes/` ⚠️ | То же: клиент/рантайм с нашим MCP |
| MIRA, Mimir, PersonalClaw, fleetmem, Khoj | См. [02](../02_findings_and_proposals.md) §8 | Референсы |

Источники: [OpenClaw architecture](https://clawdocs.org/architecture/overview), [OpenClaw scaling notes](https://rentierdigital.xyz/blog/openclaw-architecture-scaling-guide), [Hermes Agent docs](https://hermes-agent.org/docs/).

## 6. Поверхности доступа: MCP-клиенты

| Клиент | Кастомный remote MCP | Ограничения |
|---|---|---|
| **ChatGPT** | Подключение **только в вебе** («Can I connect custom MCP servers on mobile? No — web only») ✅. Business / Enterprise / Edu: полный MCP, включая write ✅. **Pro: только read/fetch** ✅. Plus в статье не упомянут ✅ | Нужен публичный HTTPS или OpenAI Secure MCP Tunnel ✅; инструменты фиксируются снапшотом, пока админ не обновит ✅; OAuth/OIDC с refresh tokens (`offline_access`) ✅. Можно ли пользоваться добавленным в вебе коннектором с телефона — не ясно ⚠️ |
| **Claude** (web / desktop / mobile) | Custom connectors на Free (1 шт.) / Pro / Max / Team / Enterprise ✅; добавляются на claude.ai, в мобильном приложении можно пользоваться уже добавленными ⚠️ | OAuth |
| Claude Code, Codex CLI, Cursor, VS Code | Да (stdio / HTTP) | — |
| OpenClaw, Hermes | Да | — |

**MCP Apps** (официальное расширение с 2026-01-26): инструмент объявляет UI через `_meta.ui.resourceUri` → `ui://`, есть деградация в текст. Поддержка: Claude (web, desktop), ChatGPT, Goose, VS Code ✅. ChatGPT по сторонним данным пока не поддерживает вызов инструментов из UI ⚠️, то есть кнопка «Approve» внутри карточки в ChatGPT может не работать.

Источники: [OpenAI: developer mode и MCP](https://help.openai.com/en/articles/12584461-developer-mode-and-full-mcp-connectors-in-chatgpt-beta), [Claude custom connectors](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp), [MCP Apps launch](https://blog.modelcontextprotocol.io/posts/2026-01-26-mcp-apps/), [MCP Apps: ChatGPT/Claude quirks](https://alpic.ai/blog/mcp-apps-goes-official-claude-chatgpt-support).

## 7. MCP-спецификация (что важно для нашего сервера)

- **2025-11-25:** CIMD (Client ID Metadata Documents) стал рекомендуемой регистрацией клиента; OIDC Discovery; incremental scope consent; экспериментальные Tasks ✅.
- **2026-07-28 (текущая):** протокол **stateless**: нет `initialize`, нет `Mcp-Session-Id`, появился `server/discover`; Tasks вынесены в расширение `io.modelcontextprotocol/tasks` (`tasks/get`, `tasks/update`); паттерн MRTR (`input_required`) заменил server-initiated elicitation/sampling; **DCR объявлен deprecated в пользу CIMD**; Roots / Sampling / Logging deprecated; добавлено поле `extensions` в capabilities ✅.
- Вывод: сервер stateless, состояние — через явные handles в аргументах; auth поддерживает CIMD + DCR (клиенты отстают от спеки); долгие операции (например, «исследуй тему») оформляются через Tasks-расширение или через наш Proposal / work item с polling.

Источник: [MCP changelog 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/changelog).

## 8. Auth для remote MCP

- ChatGPT и Claude ходят на remote MCP через OAuth. Нужен Authorization Server с CIMD (предпочтительно) и/или DCR, плюс refresh tokens.
- **FastMCP:** `OAuthProxy` / `OIDCProxy` дают DCR-подобный фасад поверх провайдеров без DCR (GitHub, Google, Azure, Auth0) и наследуют поддержку CIMD ⚠️ (доки смешивают v2 и v3). `RemoteAuthProvider` — для провайдеров с DCR.
- Keycloak: экспериментальный CIMD с ~апреля 2026 ⚠️. Pocket ID / Authentik: поддержку CIMD/DCR не проверял ⚠️.
- Для «минимум логинов» в личной инсталляции: встроенный однопользовательский AS с passkey; в корпоративной — федерация с корп. SSO.

Источники: [FastMCP OAuth proxy](https://gofastmcp.com/servers/auth/oauth-proxy), [FastMCP OIDC proxy](https://gofastmcp.com/servers/auth/oidc-proxy), [WorkOS: CIMD](https://workos.com/blog/client-id-metadata-documents-cimd-oauth-client-registration-mcp).

## 9. Источники данных

- **Slack.** Официальный MCP-сервер `https://mcp.slack.com/mcp` (Streamable HTTP): поиск сообщений и файлов с фильтрами, история каналов, треды; подключение одобряет админ воркспейса ⚠️. Свой коннектор через Web API: у internal customer-built apps прежние лимиты; у коммерчески распространяемых вне Marketplace `conversations.history` урезан до **1 запроса/мин и 15 объектов** (для старых установок с 2026-03-03) ✅.
- **Корпоративный поиск:** готовые MCP — Glean, YouTrack, Notion. Для корпоративной инсталляции это быстрый путь к источникам без своих коннекторов.
- **Документы:** Google Drive API / rclone; парсинг через Docling, MarkItDown.

Источники: [Slack MCP](https://docs.slack.dev/ai/slack-mcp-server/), [Slack rate limits change](https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps), [Slack FAQ](https://api.slack.com/changelog/2025-05-terms-rate-limit-update-and-faq).

## 10. Голос и встречи

- **STT self-hosted:** Parakeet TDT 0.6B v3 — лидер по скорости и точности для английского и 25 европейских языков (есть ли русский — проверить ⚠️); Whisper large-v3 / large-v3-turbo — 99+ языков. Voxtral с диаризацией доступен только через API Mistral ⚠️. Диаризацию в self-host делаем отдельным шагом (pyannote / WhisperX) ⚠️.
- **STT облачный:** OpenAI / Deepgram / AssemblyAI / ElevenLabs — провайдеры контракта `media.transcribe`.
- **Встречи:** **Vexa** (Apache 2.0): бот подключается к Meet / Teams / Zoom, отдаёт транскрипт с указанием спикеров по API, WebSocket, вебхуки, есть MCP-сервер, self-host ✅; Zoom требует одобрения приложения ⚠️. Плюс нативные транскрипты Meet и Zoom.

Источники: [STT сравнение](https://northflank.com/blog/best-open-source-speech-to-text-stt-model-in-2026-benchmarks), [Parakeet vs Whisper](https://openwhispr.com/blog/parakeet-vs-whisper-vs-nemotron), [Voxtral / local STT](https://wz-it.com/en/blog/local-speech-to-text-gdpr/), [Vexa](https://github.com/Vexa-ai/vexa).

## 11. Уведомления и запрос решения

- **ntfy** (self-hosted push): до 3 action-кнопок, типы `view` / `http` / `broadcast` (только Android) / `copy` ✅. Кнопка `http` может дёрнуть наш endpoint «approve / reject» прямо с экрана блокировки. Поддержку действий на iOS не проверял ⚠️.
- **Telegram bot:** inline-клавиатуры с callback, голосовые сообщения → capture. Бесплатно.
- **Slack bot** — для корпоративной инсталляции. **Apprise** — fan-out по множеству каналов.

Источник: [ntfy publish docs](https://docs.ntfy.sh/publish/).

## 12. LLM gateway

- **LiteLLM:** единый OpenAI-совместимый API, бюджеты, логирование. **24 марта 2026 версии 1.82.7 / 1.82.8 на PyPI были скомпрометированы** (кража ключей, SSH, облачных креденшелов); официальный Docker-образ прокси не пострадал ✅. Вывод: pin версий, образ вместо pip, либо свой тонкий адаптер.
- Альтернативы: свой адаптер на официальных SDK, OpenRouter (хостед), Ollama / vLLM для локальных моделей.

Источники: [BleepingComputer](https://bleepingcomputer.com/news/security/popular-litellm-pypi-package-compromised-in-teampcp-supply-chain-attack), [NetSPI](https://www.netspi.com/blog/executive-blog/ai-ml-pentesting/litellm-supply-chain-compromise/).

## 13. Платформа, деплой, бэкапы (кратко; подробно в 02 §10–11)

- Деплой: Docker Compose + Caddy (TLS) + Tailscale (админ-доступ); позже Helm; OpenTofu для VM.
- Политики: свои scopes → OPA / Cedar / OpenFGA при росте.
- Бэкапы: pg_dump или pgBackRest / WAL-G + restic или Kopia → два независимых S3 (B2 + R2 / Hetzner Storage Box); логический экспорт JSONL / Markdown в приватный git. Детали по R2 Bucket Locks и B2 Object Lock — в 02 §11.
- Object store self-host: Garage / SeaweedFS; статус и лицензию community-версии MinIO проверить ⚠️.

## 14. Rust для ядра (проверено 2026-10-17, по вопросу Q3)

| Кусок | В Rust | Заметки |
|---|---|---|
| MCP-сервер | **rmcp** — официальный Rust SDK, линейка 3.x (видел до 3.5.1); feature `auth` (OAuth 2.0), server-side Streamable HTTP, документация ссылается на спеку 2026-07-28 ✅ | Сторонняя сводка называла его «Tier 3 по conformance» — вероятно устарела ⚠️ |
| HTTP API, БД | axum, sqlx — зрелые | — |
| Фоновые задачи | **apalis**: очередь с бэкендами Postgres / MySQL / SQLite и персистентным cron; стабильная 0.7, 1.0-rc.4 (фев. 2026) ✅. Restate: Rust SDK, но лицензия сервера, по сторонним данным, BUSL ⚠️. Absurd — чистый SQL, клиент на Rust пишется | Для нашей модели (ожидание вердикта = данные, не процесс) достаточно очереди + cron + истории |
| Агентные рантаймы | Codex CLI сам написан на Rust; Claude Agent SDK — Python / TS; OpenCode — HTTP-сервер | Рантаймы всё равно внешние процессы → язык ядра не важен |
| ML-модули (STT, парсинг документов, Graphiti) | Нет нормальных Rust-аналогов | Отдельные процессы на Python за HTTP-контрактом |
| LLM-вызовы | Прямой HTTP к API провайдеров, крейты-обёртки | Несложно |

Вывод: Rust-ядро реалистично, если модули общаются по протоколам (это и так требование открытой платформы). Цена — медленнее итерации и меньше готовых библиотек вокруг AI.
Источники: [rmcp 3.1.0](https://docs.rs/crate/rmcp/3.1.0/source/README.md), [rmcp 3.5.1](https://docs.rs/crate/rmcp/3.5.1), [apalis](https://lib.rs/crates/apalis), [apalis cron guide](https://cronuru.com/guides/apalis-rust), [Restate analysis](https://www.airframe.ai/product/restate-dev/analysis).

## 15. Добавлено в раунде 3 (2026-10-17)

- **MCP и вебсокеты:** в спеке только stdio и Streamable HTTP; WebSocket-транспорт предложен (SEP-1288), но не принят ⚠️ [SEP-1288](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1288).
- **Sign in with Apple для веба** требует платного Apple Developer Program (Services ID) ⚠️ (сторонние доки: [Serverpod](https://docs.serverpod.dev/concepts/authentication/providers/apple/setup), [Akamai](https://techdocs.akamai.com/identity-cloud/docs/the-sign-in-with-apple-social-login-configuration-guide)).
- **ChatGPT и Claude поддерживают CIMD и DCR** ✅ — подробности в [12 §9](12_api_mcp_auth.md).
- **Claude Managed Agents:** MCP-серверы объявляются в агенте по URL, секреты — при создании сессии через vault ✅ [MCP connector](https://platform.claude.com/docs/en/managed-agents/mcp-connector).

## 16. Добавлено в раунде 4 (2026-10-18)

- **JetBrains Air:** облачные агенты в контейнерах, автоматизации по расписанию и событиям без вашей машины, облачные задачи тратят кредиты JetBrains AI (свои ключи — только локально), результат пушится в ветку после каждого хода, приостановленную задачу можно продолжить до архивации через 24 ч ✅ [Cloud agents](https://www.jetbrains.com/help/air/cloud-agents.html). Агенты: Claude Agent, Codex, Gemini CLI, Junie ✅. Air CLI (`aircli cloud run/status/cancel`, `aircli remote prompt/resolve/cancel/stop`; сейчас только echo-сессии) ✅. Публичного API, кроме CLI, не нашёл ⚠️.
- **Подписка Claude для фона:** `claude setup-token`; с 15.06.2026 отдельный лимит на программное использование; Agent SDK — только API-ключ; нельзя пускать чужие запросы ⚠️ (сторонние источники).
- **Codex:** `codex exec resume` ✅; вход через ChatGPT, device auth ⚠️; app-server по WebSocket ✅.
- **Claude Agent SDK:** streaming input с прерываниями и очередью сообщений ✅.

## 17. Добавлено в раунде 5 (2026-10-18)

- **OpenCode:** open source, бесплатный; платные опции команды — Zen (оплата по запросам) и Go (~$10/мес) ⚠️. С апреля 2026 Anthropic запретил расходовать подписку Claude через сторонние инструменты, включая OpenCode ⚠️ [OpenCode Go/Zen](https://pasqualepillitteri.it/en/news/10943/leaving-claude-max-codex-for-opencode-go), [ограничение подписок](https://letsdatascience.com/news/anthropic-restricts-claude-subscriptions-from-openclaw-acces-2ee184b2).
- **STT для смешанной речи:** Deepgram Nova-3 Multilingual — русский с переключением на английский ✅ (вендор); gpt-4o-transcribe-diarize — до 4 известных спикеров ✅; ElevenLabs Scribe v2 — лидер самоотчётного бенчмарка ⚠️. Подробнее в [14 §3](14_ingestion.md).

## 18. Добавлено в раунде 6 (2026-10-18)

- **OpenClaw как канал:** `openclaw mcp serve` даёт MCP-клиентам `messages_send`, `events_poll` и др. ✅ [mcp serve](https://docs.openclaw.ai/cli/mcp/serve) — наш модуль уведомлений может слать сообщения через каналы OpenClaw; внешние MCP подключаются к OpenClaw плагинами ⚠️.
- **ntfy на iOS:** кнопки действий поддерживаются, но `http`-действие на iPhone не подтверждено ⚠️.
