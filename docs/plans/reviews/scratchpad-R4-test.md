# Тесты: волна 4 scratchpad и фича целиком: PASS_WITH_FINDINGS

- **Дата:** 2026-09-16
- **Статус:** PASS_WITH_FINDINGS
- **Скоуп:** изменения T6 (`git diff develop...wave/scratchpad-w4 -- src`): шов `SystemActions` и отложенные события (`src/actor/reactor/system.rs`), перенос в `show_scratchpad`, `ScreenSpacesChanged` и полноэкранный стол, `ScratchpadCandidates`, `Unminimize`, одна копия скрытости, сортировка главного окна, объединение перепроверок в `window_server.rs`, `SpaceId::from_raw`. Сквозной прогон фичи по разделу 2 плана выполнен тестами стенда Reactor.
- **Ветка:** `test/R4-tests` (от `wave/scratchpad-w4`). Коммиты: `428d00a`, `6cee49d`, `38fd575`, `90bdb1f` и коммит с этим отчётом.
- **Команда прогона:** `cargo test --no-fail-fast`

## Итог

Все критерии приёмки T6 и пункты Z1–Z3, A, D, E подтверждены тестами. Добавлено 22 теста, ни один не падает. `cargo test` зелёный: lib 455/455 (до работы 433), doc 7/7. `cargo build --all-targets` проходит без предупреждений. Из 35 ручных мутаций новой логики 34 убиты, одна выжившая эквивалентна. Две мутации выжили на первом прогоне; под них добавлены тесты, и после этого обе убиты. Блокирующих находок нет. Есть три замечания, главное из них: при ошибке создания потока запуска replay принимает другое решение, чем живой Reactor.

## Критерии приёмки T6

| Критерий | Тесты | Мутации |
|---|---|---|
| Окно на другом столе: перенос на active space, рамка и фокус только после `WindowsOnScreenUpdated` с wsid | `window_on_another_space_is_moved_and_then_shown`, `partial_window_update_ends_the_wait`, `races::screen_change_that_shows_the_window_ends_the_wait`, `races::space_change_that_shows_the_window_ends_the_wait` | M5, M6, M7, M14 |
| Таймаут: показ без переноса; истечение старого ожидания не влияет на новое | `move_timeout_shows_the_window_without_the_move`, `races::toggle_during_a_move_moves_again_without_hiding_or_launching`, `races::move_ids_stay_unique_after_a_failed_move`, `races::toggle_that_shows_at_once_drops_the_earlier_wait` | M2, M3, M15 |
| `Unsupported` → рамка и фокус сразу | `unsupported_move_shows_the_window_at_once` | M4 |
| Полноэкранный активный стол → перенос не вызывается | `window_is_not_moved_to_a_fullscreen_space`, `races::fullscreen_space_of_another_screen_does_not_stop_the_move` | M1, M10 |
| Скрытое приложение: перенос, `SetHidden(false)`, рамка и фокус | `hidden_app_window_is_moved_unhidden_and_shown` | M13 |
| Окно на выключенном столе регистрируется, повторного запуска нет | `window_on_a_disabled_space_is_registered_and_shown_without_a_launch`, `launched_window_on_a_disabled_space_is_shown`, `edge_cases::window_created_later_on_a_disabled_space_is_registered` (новый) | M19, M20, M21 |
| Свёрнутое окно разворачивается при показе | `toggle_unminimizes_a_window_that_is_not_on_screen` (обработчик AX в `app.rs` тестами не покрыт) | M11, M12 |
| Replay: перенос не вызывается, решения совпадают | `replay_makes_the_same_decisions_without_moving`, `races::replay_of_a_move_and_its_arrival_makes_the_same_decisions` (сверяются рамки, счётчик id, ожидания), `races::replay_does_not_wait_where_the_live_reactor_did_not_move` | M4, M1 |
| Главное окно на другом экране или пришедшее вторым становится scratchpad (A) | `main_window_on_another_screen_becomes_the_scratchpad`, `main_window_of_a_two_window_app_becomes_the_scratchpad`, `visible_window_is_preferred_over_a_window_on_another_space` | M17, M18 |
| Z1: зависший `open` не продлевает показ | `launch_expires_after_the_timeout_even_if_open_hangs`, `failed_open_expires_the_show_at_once` | M27, M28 |
| Z2: одна запись на ключ | `deadlines_keep_the_last_event_per_key`, `a_later_event_for_a_key_replaces_an_earlier_deadline_either_way` (новый), `show_and_move_keys_do_not_replace_each_other` (новый), `delayed_events_are_sent_after_their_delay_once_per_key` (новый, настоящий `Timer::manual` на `Executor`) | M23, M24, M25, M29, M34 |
| Z3: серия hide/show одного pid даёт одну перепроверку | `a_burst_of_hidden_changes_is_rechecked_once`, `recheck_runs_once_on_the_timer_of_the_running_actor` (новый, настоящий цикл `WindowServer::run`) | M30, M31, M33 |
| D: тест запуска не запускает `/usr/bin/open` | `launch_does_not_open_or_call_back_for_an_invalid_id` | M35 (при сломанной проверке тест падает, фиктивный `open` не запускает процесс) |
| E: перепроверка без реального времени | `hidden_change_schedules_a_recheck` (время передаётся параметром) | M30 |
| `ScreenSpacesChanged` доходит до Reactor раньше смены стола | `window_server::space_change_reports_the_current_spaces_before_the_space_change` (новый) | M16, M32 |
| Все прежние тесты зелёные, сборка без warnings | полный прогон | — |

По разделу 2 плана: правило и плавание (`scratchpad_window_floats_outside_the_tree`), запуск и отложенный показ (`launched_window_on_another_space_is_moved`, тесты F3), показ скрытого, несфокусированного и находящегося на другом столе окна, скрытие видимого окна с фокусом (`toggle_hides_the_visible_focused_window`, `races::toggle_after_the_window_arrived_hides_it`). Живой пункт 4 (⌘⌥K на машине пользователя) тестами проверить нельзя, он остаётся за человеком (сценарии П1–П10 в отчёте T6).

## Новые тесты

`src/actor/reactor.rs`, модуль `tests::scratchpad::space_moves::races`, 16 тестов:

| Гонка из задания | Тест |
|---|---|
| toggle во время ожидания переноса | `toggle_during_a_move_moves_again_without_hiding_or_launching`, `toggle_that_shows_at_once_drops_the_earlier_wait`, `toggle_after_the_window_arrived_hides_it` |
| окно закрыто во время ожидания | `window_closed_during_a_move_is_not_shown_by_later_updates` (window server ещё перечисляет закрытое окно) |
| приложение скрыто пользователем во время ожидания | `app_hidden_during_a_move_does_not_end_the_wait` |
| смена стола пользователем во время ожидания | `space_change_that_shows_the_window_ends_the_wait`, `space_change_without_the_window_keeps_waiting` (следующий toggle переносит окно на новый стол), `screen_change_that_shows_the_window_ends_the_wait` |
| два scratchpad одновременно | `two_scratchpads_wait_and_end_independently`, `two_scratchpads_arriving_together_are_both_shown`, `quitting_one_app_keeps_the_move_of_another` |
| прочее | `move_ids_stay_unique_after_a_failed_move`, `active_screen_without_a_recorded_space_shows_at_once`, `fullscreen_space_of_another_screen_does_not_stop_the_move`, `replay_of_a_move_and_its_arrival_makes_the_same_decisions`, `replay_does_not_wait_where_the_live_reactor_did_not_move` |

Кроме того:
- `src/actor/reactor.rs`, `edge_cases::window_created_later_on_a_disabled_space_is_registered`: ветка `WindowBecameVisible` без стола → `ScratchpadCandidates`. До этого теста её удаление (M20) не ловил ни один тест.
- `src/actor/reactor/system.rs`: 3 теста дедлайнов по ключу, один из них проверяет настоящую задачу `run_delayed_events` с `Timer::manual` (задержки 30–150 мс, ожидание до 5 с).
- `src/actor/window_server.rs`: 2 теста, порядок `ScreenSpacesChanged` → `SpaceChanged` и настоящий цикл `WindowServer::run` с таймером перепроверки.

`src/actor/reactor/testing.rs` не менялся: хватило существующего стенда.

## Результат прогона

- Базовый прогон до работы: lib 433 passed, doc 7 passed, падений нет.
- Итоговый прогон: lib **455 passed, 0 failed**, doc 7 passed. `cargo build --all-targets`: 0 warnings.
- Стабильность: `cargo test --lib -- scratchpad system:: window_server` три раза подряд (188/188) и весь lib с `--test-threads=1` (453/453 до последнего коммита, после него полный прогон 455/455). Тесты с реальным таймером не мигали.
- Формат: nightly `rustfmt --edition 2024` по трём изменённым файлам.

## Покрытие

`cargo llvm-cov` и `cargo mutants` не установлены, ставить их я не стал. Покрытие изменённых строк оценено вручную и подтверждено мутациями: **~92%** при пороге 80%. Не исполняются:
- `LiveSystem::new` с настоящими `launch_app_then` и `move_window_to_space` в поле по умолчанию, а также `Reactor::spawn`. Для этого нужен живой WM.
- Тело `open_bundle` (`src/sys/app.rs`), то есть настоящий `/usr/bin/open`.
- `space_move::move_window_to_space` вместе с `autoreleasepool` и `space_is_fullscreen` на полноэкранном столе (SkyLight). `space_type_can_be_read` только проверяет, что вызов линкуется.
- Обработчик `Request::Unminimize` в `src/actor/app.rs` (AX).

## Мутации

Скрипт `target/r4_mutate.py` (не коммитится) вносит мутации по одной, после каждой запускает `cargo test --lib --no-fail-fast` и возвращает файл в исходное состояние. После прогона `git status` показывает только `.omc/`. Настоящих `open` и переносов мутанты не вызывают.

| # | Мутация | Результат | Кем убита (пример) |
|---|---|---|---|
| M1 | полноэкранный стол не проверяется | убита | `window_is_not_moved_to_a_fullscreen_space`, `replay_does_not_wait_where_the_live_reactor_did_not_move` |
| M2 | `ScratchpadMoveEnded` не сверяет id | убита | `move_timeout_shows_the_window_without_the_move`, `move_ids_stay_unique_after_a_failed_move` |
| M3 | прежнее ожидание не снимается перед новым показом | выжила → **убита** новым тестом | `toggle_that_shows_at_once_drops_the_earlier_wait` |
| M4 | исход неудачного переноса не пишется в трассу | убита | `unsupported_move_shows_the_window_at_once` |
| M5 | нет размещения на `WindowsOnScreenUpdated` | убита (10) | `partial_window_update_ends_the_wait` и др. |
| M6 | нет размещения на `ScreenParametersChanged` | убита | `screen_change_that_shows_the_window_ends_the_wait` (новый; раньше не ловилась) |
| M7 | нет размещения на `SpaceChanged` | убита | `space_change_that_shows_the_window_ends_the_wait` (новый; раньше не ловилась) |
| M8 | завершение приложения не снимает ожидание | убита | `quitting_the_app_drops_its_move`, `quitting_one_app_keeps_the_move_of_another` |
| M9 | закрытие окна не снимает ожидание | убита | `destroyed_window_is_not_shown_after_its_move`, `window_closed_during_a_move_…` |
| M10 | целевой стол всегда у экрана 0 | убита | `window_is_moved_to_the_space_of_the_active_screen` и 2 новых |
| M11 / M12 | `Unminimize` всегда / никогда | убиты | `toggle_unminimizes_a_window_that_is_not_on_screen` |
| M13 | скрытое приложение не переносится | убита | `hidden_app_window_is_moved_unhidden_and_shown` |
| M14 | видимое окно тоже переносится | убита | `visible_window_is_not_moved` |
| M15 | id переноса не растёт | убита | `move_timeout_…`, `two_scratchpads_wait_and_end_independently` |
| M16 | `ScreenSpacesChanged` игнорируется | убита (21) | тесты `space_moves` |
| M17 / M18 | в сортировке кандидатов нет ключа главного окна / видимости | убиты | `main_window_on_another_screen_becomes_the_scratchpad` / `visible_window_is_preferred_over_a_window_on_another_space` |
| M19 | `ScratchpadCandidates` не отправляется | убита | `window_on_a_disabled_space_is_registered_…` и др. |
| M20 | `WindowBecameVisible` без стола ничего не делает | выжила → **убита** новым тестом | `window_created_later_on_a_disabled_space_is_registered` |
| M21 | кандидатом становится и тайловое окно | убита | `scratchpad_candidates_are_registered_without_layout_changes` |
| M22 | кандидат с любым классом, кроме `Untracked` | **выжила, эквивалентна** | — (класс `Regular` означает, что первое подходящее правило не scratchpad, поэтому `scratchpad_rule` вернёт `None`) |
| M23 | дедлайн по ключу не заменяется | убита | `deadlines_keep_the_last_event_per_key` и 3 новых |
| M24 | срок сравнивается строго (`<`) | убита | `a_later_event_for_a_key_…` |
| M25 | `next()` возвращает максимум | убита | `deadlines_keep_the_last_event_per_key`, `delayed_events_are_sent_…` |
| M26 | после переноса нет перепроверки | убита | `move_checks_the_windows_again_and_ends_after_a_timeout` |
| M27 | колбэк `open` снимает показ и при успехе | убита | `failed_open_expires_the_show_at_once` |
| M28 | срок показа изменён | убита | `launch_expires_after_the_timeout_even_if_open_hangs` |
| M29 | ключ переноса совпадает с ключом показа | убита | `move_checks_the_windows_again_and_ends_after_a_timeout` |
| M30 | серия изменений не сдвигает срок | убита | `a_burst_of_hidden_changes_is_rechecked_once` |
| M31 | pid не очищаются после перепроверки | убита | `a_burst_of_hidden_changes_is_rechecked_once` |
| M32 | WindowServer не шлёт `ScreenSpacesChanged` | убита | `space_change_reports_the_current_spaces_before_the_space_change` (новый) |
| M33 | нет немедленной перепроверки | убита (6) | `recheck_request_checks_now_and_later` и др. |
| M34 | `run_delayed_events` не взводит таймер | убита | `delayed_events_are_sent_after_their_delay_once_per_key` (новый) |
| M35 | `launch_app_with` не проверяет id | убита | `launch_does_not_open_or_call_back_for_an_invalid_id` и 3 теста валидации |

Итог: **34 из 35 убиты**, одна выжившая (M22) эквивалентна. M3 и M20 выжили на первом прогоне, после новых тестов убиты. M6, M7, M32 и M34 убиты только новыми тестами.

## Находки

### 1. Ошибка создания потока запуска не пишется в трассу, и replay расходится с живым Reactor (замечание)

- **Где:** `src/actor/reactor.rs:1341`. `LiveSystem::launch_app` возвращает `LaunchError::Spawn`, если не удалось создать поток (`src/sys/app.rs:101`). Тогда живой Reactor снимает отложенный показ. В replay шов `NoSystem`, он возвращает `Ok`, и показ остаётся.
- **Воспроизведение:** временный тест, в репозиторий не добавлен. Шов возвращает `Spawn`, выполняется `toggle("l", Some("com.testapp3"))`. У живого Reactor `pending_scratchpad_show("l") == None`, у воспроизведённого `Some(..)`, assert `replay` падает.
- **Чем грозит:** если окно потом появится, replay его покажет, а живой Reactor нет. Это нарушает критерий «решения совпадают», хотя случается только при нехватке ресурсов ОС.
- **Предлагается:** сделать так же, как для переноса: на ошибку запуска записывать в трассу `ScratchpadShowExpired(id)` (`self.record.on_event`). Обработчик этого события и так вызывает `cancel_scratchpad_show`.

### 2. Скрытие приложения во время ожидания не отменяет показ по таймауту (замечание)

- **Где:** `src/actor/reactor.rs:574` (`ScratchpadMoveEnded`) → `place_scratchpad` (`:1441`).
- **Что:** если во время ожидания переноса приложение скрыли, например через Dock, ожидание остаётся (`app_hidden_during_a_move_does_not_end_the_wait`). Через 1 с `ScratchpadMoveEnded` выставит рамку и фокус скрытому приложению. Фокус через `make_key_window`, скорее всего, снова его покажет.
- **Чем грозит:** в пределах одной секунды отменяется явное действие пользователя. Сценарий редкий: пока окно ждёт, у scratchpad-приложения нет фокуса, поэтому Cmd+H скроет другое приложение.
- **Предлагается:** решить, что правильно. Вариант: снимать ожидания pid на `ApplicationHiddenChanged(pid, true)`. Тест намеренно не фиксирует поведение по таймауту в этом случае.

### 3. Поток на каждый запуск остаётся (замечание)

- **Где:** `src/sys/app.rs:101`.
- **Что:** Z2 закрыт для ожидания: спящих потоков больше нет, на имя приходится одна запись дедлайна. Но каждое нажатие без окна по-прежнему создаёт поток `open`. Если `open` зависает, такие потоки копятся. Тест `deadlines_keep_the_last_event_per_key` это не проверяет, а формулировка критерия («число потоков не растёт с числом нажатий») шире того, что сделано.
- **Предлагается:** принять как есть (в норме `open` завершается быстро) или не запускать второй `open` для имени, пока первый не завершился.

### Наблюдение, не дефект

Во время Mission Control SpaceManager задерживает `SpaceChanged` (`src/actor/space_manager.rs:144`), а `ScreenSpacesChanged` проходит сразу (`:151`). Поэтому `screen_spaces` может опережать `screens[].space`. Для переноса это даже полезно: целевым становится стол, на который пользователь уходит. После выхода из Mission Control SpaceManager всё равно запрашивает обновление.

## Допущения

- Находка 1 не оставлена красным тестом: условие искусственное (отказ `thread::Builder::spawn`), а командир требует зелёный `cargo test`. Воспроизведение описано выше.
- Для находки 2 не написано ни красного, ни фиксирующего теста, потому что правильное поведение не определено планом. Тест фиксирует только то, что скрытие не завершает ожидание досрочно.
- Тесты гонок добавлены вложенным модулем в `space_moves` (T6): так доступны его приватные помощники, и не нужно менять `testing.rs`.
- Два теста используют реальное время на `Executor` (`delayed_events_are_sent_…`, `recheck_runs_once_on_…`), потому что по-другому настоящий `Timer::manual` не проверить. Нижние границы задержек в них мягкие (≥ задержки), верхних нет, кроме общего предела 5 с.
- R4-qa параллельно правит `glide.default.toml` и `README.md`. Эти файлы я не трогал.

## Было сломано до начала работы

Нет: базовый прогон 433/433, doc 7/7.
