# Тесты: волна 2 (T4, скрытие приложения) — PASS_WITH_FINDINGS

- **Дата:** 2026-09-16
- **Статус:** PASS_WITH_FINDINGS
- **Скоуп:** волна 2 плана `docs/plans/2026-09-16-scratchpad.md`, изменения T4 (`git diff develop...wave/scratchpad-w2 -- src`): `src/actor/app.rs`, `src/actor/reactor.rs`, `src/actor/reactor/testing.rs`, `src/actor/reactor/main_window.rs`, поле `AppInfo::is_hidden` в `src/sys/app.rs`
- **Ветка:** `test/R2-tests` (от `wave/scratchpad-w2`)
- **Команда прогона:** `cargo test`

## Итог

Все критерии приёмки T4 подтверждены тестами. 11 новых тестов проходят, упавших нет. Каждая из 13 внесённых мутаций (в Reactor, в `serde(default)` и в стенде) роняет хотя бы один тест. Блокирующих находок нет. Код потока приложения (`src/actor/app.rs`) работает только с живым AX, поэтому тестами не покрыт. Его нужно проверить руками на машине пользователя: Cmd+H и `SetHidden`. Остальные четыре находки — замечания к тестовому стенду и к покрытию, они пригодятся для T5.

## Что покрыто

Все тесты лежат в `src/actor/reactor.rs`, модуль `tests`. Хелперы трассы добавлены в `src/actor/reactor/replay.rs`, модуль `#[cfg(test)] tests`.

| Тест | Что проверяет |
|------|---------------|
| `repeated_hidden_changed_events_keep_the_last_state` | повторные hide/hide и show/show не ломают состояние; событие не порождает запросов и не меняет рамки окон |
| `hidden_state_is_tracked_per_app` | три приложения с разным начальным `AppInfo::is_hidden`; событие для одного pid не задевает другие |
| `hidden_state_survives_application_terminated_until_thread_exits` | после `ApplicationTerminated` флаг сохраняется и уходит `Request::Terminate`; после `ApplicationThreadTerminated` возвращается `false`; запоздавшее событие для завершённого pid не создаёт запись |
| `relaunched_app_with_same_pid_starts_from_new_launch_info` | перезапуск с тем же pid берёт значение из нового `AppInfo`, скрытое или показанное, а не старое |
| `relaunched_app_with_new_pid_is_independent_of_old_pid` | перезапуск с другим pid: события для старого pid игнорируются и не влияют на новый |
| `harness_attributes_set_hidden_to_the_app_it_was_sent_to` | стенд с тремя приложениями и запросами вперемешку (3, 1, 3): `tagged_requests` правильно ставит pid, ответы приходят с правильным pid, `Apps::hidden` хранит последнее значение |
| `harness_simulate_events_answers_set_hidden` | старый путь `simulate_events()` отвечает на `SetHidden` с правильным pid, запросы читаются один раз |
| `harness_untagged_requests_still_see_every_app` | прежний `requests()` по-прежнему собирает запросы всех приложений (сгруппированы по pid) |
| `harness_untagged_simulation_rejects_set_hidden` | `simulate_events_for_requests` на `SetHidden` паникует с понятным сообщением |
| `hidden_changed_event_is_recorded_and_replayed` | `ApplicationHiddenChanged` и `is_hidden:true` попадают в трассу; после воспроизведения у Reactor то же состояние скрытости |
| `old_trace_without_hidden_field_replays_as_shown` | трасса, из которой вырезано поле `is_hidden` (так выглядит старая трасса), воспроизводится, приложение считается показанным; та же трасса с полем восстанавливает `true` |

Эти тесты дополняют четыре теста T4 (`app_hidden_state_follows_hidden_changed_events`, `app_hidden_state_starts_from_launch_info`, `harness_answers_set_hidden_with_hidden_changed`, `app_info_without_hidden_field_deserializes_as_shown`).

Тестовая инфраструктура, только добавления: в `replay::tests` появились `recorded_trace(&mut Reactor)` (читает трассу, которую записал тестовый Reactor) и `replay_trace(&str) -> Reactor`. Второй хелпер воспроизводит трассу так же, как `replay`, но возвращает Reactor, чтобы можно было проверить итоговое состояние. `Drop` в `testing.rs` проверяет только, что воспроизведение не падает. `src/actor/reactor/testing.rs` не менялся.

Добавлено тестов: 11, все интеграционные на стенде Reactor.

## Результат прогона

- Базовый прогон (`wave/scratchpad-w2`, до работы): lib 304 passed, doc 7 passed, 0 failed
- Итоговый прогон: lib 315 passed, doc 7 passed, 0 failed, без warnings
- Стабильность: новые тесты прогонялись 15 раз (полный прогон, прогон по фильтру и 13 прогонов мутаций). Вне мутаций результат всегда одинаковый. Время, случайность и порядок в тестах не используются.

Падающих тестов нет.

## Покрытие

Инструмента покрытия в проекте нет (`cargo llvm-cov` не установлен). Ставить его ради одного прогона не стал, поэтому покрытие посчитано вручную по исполняемым изменённым строкам. Что строки действительно выполняются, подтверждают мутации.

- `src/actor/reactor.rs` (продакшн-часть): вариант события, `AppState::is_hidden`, копирование из `AppInfo`, обработка события, `is_app_hidden`. Покрыто всё.
- `src/actor/reactor/testing.rs`: каналы по pid, `tagged_requests`, `simulate*`, ответ на `SetHidden`. Покрыто всё.
- `src/actor/reactor/main_window.rs:80`: ветка, которая игнорирует событие. Покрыта, через неё проходит каждое `ApplicationHiddenChanged`.
- `src/sys/app.rs:135-136`: `serde(default)` покрыт. `is_hidden: app.isHidden()` в `From<&NSRunningApplication>` (`src/sys/app.rs:144`) не покрыт, это FFI.
- `src/actor/app.rs`: не покрыт.

Итого покрыто примерно 77% изменённых исполняемых строк (около 40 из 52) при пороге 80%. Непокрытые строки и причина:

- `src/actor/app.rs:480`: начальное чтение `sys::app::is_app_hidden`.
- `src/actor/app.rs:553-558`: подписка на `OPTIONAL_APP_NOTIFICATIONS`.
- `src/actor/app.rs:734-738`: выполнение `Request::SetHidden` через `set_app_hidden` и `warn!`.
- `src/actor/app.rs:752-755`: перевод уведомления hide/show в `ApplicationHiddenChanged`.
- `src/sys/app.rs:144`: `NSRunningApplication::isHidden`.

Все эти строки работают только с живым процессом через AX или AppKit. У потока приложения нет тестового стенда, а по условиям задачи скрывать реальные приложения нельзя. Из кода проверен порядок: подписка на hide/show (`register_app_notifs`) происходит до чтения `is_hidden` (строка 480). `ApplicationLaunched` и `ApplicationHiddenChanged` идут через один канал `ws_tx` и WindowServer (`window_server.rs:89-93`, `184`), поэтому скрытие сразу после запуска не может прийти в Reactor раньше `ApplicationLaunched`.

## Находки

### 1. Поток приложения не покрыт тестами — замечание

- **Где:** `src/actor/app.rs:480`, `553-558`, `734-738`, `752-755`
- **Чем грозит:** если уведомления hide/show на macOS 27 не приходят или `set_app_hidden` не срабатывает, ни один тест этого не покажет. Это риск из раздела 10 плана.
- **Предлагается:** на T5 или приёмке проверить руками на машине пользователя: Cmd+H у Keyguard, затем команда scratchpad, и посмотреть в логе `RUST_LOG=debug`, приходит ли `ApplicationHiddenChanged`.

### 2. `Request::Terminate` в стенде отбрасывает запросы других приложений из той же пачки — замечание

- **Где:** `src/actor/reactor/testing.rs:250` (`Request::Terminate => break`)
- **Чем грозит:** `tagged_requests()` группирует запросы по pid. Поэтому `Terminate` для приложения с меньшим pid молча отбрасывает все уже прочитанные запросы приложений с большими pid, включая `SetHidden`, и стенд на них не отвечает. До T4 так же терялись запросы, отправленные позже `Terminate`; теперь теряются запросы с большими pid. Сейчас ни один тест на это не опирается. В T5 тест, где одно приложение завершается, а другое скрывается, может получить непонятный результат.
- **Предлагается:** в стенде пропускать только запросы того pid, который получил `Terminate` (`continue` с фильтром по pid). Это не аддитивная правка стенда, поэтому оставил её для F2 или T5.

### 3. `requests()` больше не сохраняет порядок запросов между приложениями — замечание

- **Где:** `src/actor/reactor/testing.rs:200-213`
- **Чем грозит:** тесты больше не видят, в каком порядке Reactor отправлял запросы разным приложениям. Ошибка порядка (например, raise до show) пройдёт незамеченной. В отчёте T4 это описано.
- **Предлагается:** если в T5 порядок «показать → поднять окно» важен, проверять его в пределах одного pid или добавить в стенд общий журнал запросов с порядковыми номерами.

### 4. Стенд отвечает на каждый `SetHidden`, даже если состояние не изменилось — замечание

- **Где:** `src/actor/reactor/testing.rs:320-324`
- **Чем грозит:** настоящая macOS не шлёт уведомление при повторном hide. Для Reactor сейчас это не важно, обработка идемпотентна, что проверяет `repeated_hidden_changed_events_keep_the_last_state`. Но если T5 начнёт реагировать на `ApplicationHiddenChanged` действиями (например, фокусом), стенд покажет лишние события.
- **Предлагается:** учесть это в T5. При необходимости отвечать только на изменение `Apps::hidden`.

## Чувствительность тестов

Каждая мутация вносилась локально через `perl` и откатывалась `git checkout -- <файл>`. После всех прогонов `git status` показывает только неотслеживаемые `.omc/`, продакшн-файлы чисты. Скрипт: `mutate.sh` в scratchpad сессии. Прогон: `cargo test --lib actor::reactor`.

| # | Мутация | Где | Упало тестов | Среди упавших |
|---|---------|-----|--------------|---------------|
| M1 | обработчик `ApplicationHiddenChanged` ничего не делает | reactor.rs:524 | 9 | `app_hidden_state_follows_hidden_changed_events`, `hidden_changed_event_is_recorded_and_replayed` |
| M2 | всегда `is_hidden = true` | reactor.rs:524 | 6 | `repeated_hidden_changed_events_keep_the_last_state`, `hidden_state_is_tracked_per_app` |
| M3 | `is_hidden = !hidden` | reactor.rs:524 | 9 | `hidden_state_survives_application_terminated_until_thread_exits` |
| M4 | `is_app_hidden` для неизвестного pid → `true` | reactor.rs:1198 | 4 | `app_hidden_state_follows_hidden_changed_events`, `relaunched_app_with_new_pid_is_independent_of_old_pid` |
| M5 | `is_app_hidden` всегда `false` | reactor.rs:1198 | 11 | все тесты состояния |
| M6 | при запуске `is_hidden = false` вместо `info.is_hidden` | reactor.rs:504 | 4 | `app_hidden_state_starts_from_launch_info`, `relaunched_app_with_same_pid_starts_from_new_launch_info`, `old_trace_without_hidden_field_replays_as_shown` |
| M7 | `ApplicationThreadTerminated` не удаляет приложение | reactor.rs:519 | 3 | `hidden_state_survives_application_terminated_until_thread_exits` |
| M8 | убран `#[serde(default)]` у `AppInfo::is_hidden` | sys/app.rs:135 | 2 | `app_info_without_hidden_field_deserializes_as_shown`, `old_trace_without_hidden_field_replays_as_shown` |
| M9 | стенд отвечает `!hidden` | testing.rs:323 | 3 | `harness_attributes_set_hidden_to_the_app_it_was_sent_to`, `harness_simulate_events_answers_set_hidden` |
| M10 | стенд отвечает с pid 1 вместо pid адресата | testing.rs:323 | 3 | те же |
| M11 | стенд не записывает `Apps::hidden` | testing.rs:322 | 2 | `harness_answers_set_hidden_with_hidden_changed`, `harness_attributes_set_hidden_to_the_app_it_was_sent_to` |
| M12 | `tagged_requests` ставит всем запросам pid 1 | testing.rs:211 | 3 | `harness_attributes_set_hidden_to_the_app_it_was_sent_to` |
| M13 | стенд не отвечает на `SetHidden` | testing.rs:323 | 3 | `harness_simulate_events_answers_set_hidden` |

Итог: погибли все 13 мутаций из 13. Из 15 тестов скрытия (11 новых и 4 из T4) хотя бы одна мутация уронила 13. Не упали `harness_untagged_requests_still_see_every_app` и `harness_untagged_simulation_rejects_set_hidden`: это регрессионные проверки старого API стенда, и ни одна мутация их не затрагивала. Проверка на переспецифицированность не нужна: падающих тестов и предлагаемых исправлений нет. Mutation-тестирование инструментом (cargo-mutants) не проводилось, в проекте он не настроен.

## Допущения

- Покрытие посчитано вручную, потому что `cargo llvm-cov` не установлен. Ставить инструмент не стал: это меняло бы окружение.
- Старую трассу моделирую так: из свежей записи вырезается `,is_hidden:true`. Настоящих старых `.ron` в репозитории нет (`traces/` отсутствует).
- Чтобы проверять итоговое состояние после воспроизведения, добавил в `replay.rs` тестовый модуль, который повторяет цикл `replay()`. Если `replay()` изменится, хелпер нужно будет поправить вместе с ним. Продакшн-функцию `replay` не трогал.
- Считаю правильным, что между `ApplicationTerminated` и `ApplicationThreadTerminated` приложение сохраняет свой флаг, и зафиксировал это в тесте. Контракт этот случай не описывает, но так ведёт себя остальное состояние `AppState`.
- Находку 2 не исправлял: правка стенда была бы не аддитивной.

## Было сломано до начала работы

Нет.
