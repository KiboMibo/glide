# Тесты: волна 3 scratchpad (T5, интеграция) — FAIL

- **Дата:** 2026-09-16
- **Статус:** FAIL
- **Скоуп:** волна 3 плана `docs/plans/2026-09-16-scratchpad.md` (R3-test): `git diff develop...wave/scratchpad-w3 -- src glide.default.toml`, то есть `src/actor/layout.rs`, `src/actor/reactor.rs`, `src/actor/reactor/testing.rs`, `src/config.rs`, `src/sys/app.rs`, `glide.default.toml`. Проверялись критерии T5 (пп. 1–7) и раздела 2 плана, а также краевые случаи сверх T5.
- **Ветка:** `test/R3-tests` (от `wave/scratchpad-w3`), коммиты `9ab1755`, `d7dce25`
- **Команда прогона:** `cargo test --no-fail-fast`

## Итог

Основные сценарии раздела 2 работают, и тесты T5 их честно проверяют. Добавлено 21 интеграционный тест стенда Reactor (`actor::reactor::tests::scratchpad::edge_cases`). Из них падает один: после удаления правила из конфига окно так и остаётся scratchpad (важная находка 1). Исправление в одну строку проверено на временной правке: с ним весь набор зелёный (366/366). Остальные находки не блокируют. Главная из них: какое из нескольких окон приложения станет scratchpad, определяет порядок хеш-множества. Статус FAIL выставлен формально, по правилу «есть красный тест». Основной сценарий ⌘⌥K этот дефект не ломает, поэтому командир может взять находку 1 в F3, не останавливая волну.

## Что покрыто

| Сценарий из задания | Тесты (`edge_cases::…`) |
|---|---|
| Двойное нажатие до появления окна | `second_toggle_before_the_window_appears_shows_it_once`: окно показано один раз, повторного показа после обновления нет |
| Окно появилось на другом экране | `launched_window_on_another_screen_is_placed_on_the_active_screen`, `toggle_brings_a_window_from_another_screen_to_the_active_screen`, `focused_window_on_the_other_screen_is_hidden`, `toggle_uses_the_screen_of_the_focused_window` |
| Окно на выключенном рабочем столе / Glide выключен | `launched_window_on_a_disabled_space_is_left_alone`, `registered_scratchpad_is_shown_while_the_space_is_disabled`, `window_moved_from_a_disabled_space_is_registered` |
| Приложение с двумя окнами | `only_the_scratchpad_window_of_a_two_window_app_is_shown`, `remaining_window_of_the_app_takes_over_after_the_next_refresh` |
| Закрытие окна при скрытом приложении | `window_closed_while_hidden_is_replaced_by_a_launched_one` (запуск → `SetHidden(false)` + рамка + фокус), `window_closed_while_hidden_is_not_shown_by_the_next_window_without_toggle` |
| Изменение конфига на лету | `renamed_rule_moves_the_window_to_the_new_name_after_reload`, `changed_frame_is_used_after_reload`, `window_moved_to_another_screen_is_registered_under_the_renamed_rule`, **`removed_rule_stops_the_window_being_a_scratchpad_after_reload` (падает)** |
| Cmd+H на другом приложении | `hiding_another_app_does_not_change_the_scratchpad_decision` |
| `move_focus` / `toggle_focus_floating` при видимом scratchpad | `move_focus_does_nothing_from_a_visible_scratchpad`, `toggle_focus_floating_leaves_a_visible_scratchpad_for_the_tiled_windows` |
| Окно, спрятанное приложением за край экрана | `focused_window_parked_off_screen_is_shown` |
| Порядок правил | `first_matching_rule_decides_the_scratchpad` |

Добавлено 21 интеграционный тест, unit-тестов 0. В стенд (`testing.rs`) ничего не добавлялось: нужные помощники (`with_screens`, `refresh`, `drag`, `set_main_window`, `add_window`) лежат в тестовом модуле как `impl Test`.

## Результат прогона

- Базовый прогон (до работы, `cargo test --lib`): 345 passed, 0 failed.
- Итоговый прогон (`cargo test --no-fail-fast`): lib 365 passed, 1 failed; doc 7 passed. Warnings нет.
- Стабильность: `cargo test --lib scratchpad` запускался 5 раз, в том числе дважды с `--test-threads=1`. Результат всегда одинаковый: 99 passed, 1 failed.

Падающий тест:

- `actor::reactor::tests::scratchpad::edge_cases::removed_rule_stops_the_window_being_a_scratchpad_after_reload`: после удаления правила `scratchpad_window("k")` остаётся `Some(WindowId(2,1))`, ожидалось `None`.

## Покрытие

- Изменённые строки: **~99% (ручная оценка)**, порог 80%. `cargo llvm-cov` и tarpaulin не установлены. Оценка сделана по исполняемым строкам diff и подтверждена мутациями: из 37 мутантов не убиты только два эквивалентных.
- Не исполняется одна строка: `src/actor/reactor.rs:386` (`reactor.launcher = Box::new(crate::sys::app::launch_app)` в `Reactor::spawn`). Это настоящий запуск приложения, в тестах его вызывать запрещено.

## Находки

### 1. Удалённое правило не снимает с окна статус scratchpad — важная

- **Где:** `src/actor/layout.rs:1204-1205` (`register_scratchpad` молча выходит, если правило не найдено); `set_config` (`layout.rs:403`) реестр не трогает.
- **Чем грозит:** пользователь удаляет правило `scratchpad` (или меняет его на `float = false`) и перезагружает конфиг. Окно остаётся в реестре до закрытия: `toggle_scratchpad` со старым именем продолжает его прятать и показывать, `ToggleWindowFloating` на нём ничего не делает, и окно нельзя вернуть в раскладку. Это расходится с принципом «Config reload … only unmodified values update» и с п. 1 раздела 2: окно помечает правило, а правила уже нет.
- **Предлагается:** в `register_scratchpad` при отсутствии правила вызывать `self.scratchpads.remove_window(wid)`. Проверено на временной правке (откачена): весь `cargo test --lib` зелёный, 366/366. Тесты под эту правку менять не нужно. Правка срабатывает при обновлении окон после перезагрузки конфига (`RequestSpaceRefresh` → `GetVisibleWindows`), для окон скрытого приложения — при следующем обнаружении.
- **Тест:** `edge_cases::removed_rule_stops_the_window_being_a_scratchpad_after_reload` (падает).

### 2. Какое из нескольких подходящих окон станет scratchpad, определяет хеш-порядок — важная

- **Где:** `src/actor/reactor.rs:1001-1004` (`on_windows_discovered` обходит `visible_windows: FxHashSet`), `src/actor/layout.rs:638` (регистрация в порядке списка).
- **Чем грозит:** у приложения с двумя окнами, подходящими под правило (главное окно и мини-плеер или всплывающее окно), scratchpad может стать второстепенное окно. В стенде при окнах `(2,1)` и `(2,2)` зарегистрировалось `(2,2)`. План обещает «первое зарегистрированное», и формально так и есть, но «первое» здесь выбирается произвольно.
- **Предлагается:** регистрировать окна одного приложения в детерминированном порядке: главное окно первым, остальные по `WindowId`. Либо явно описать в `glide.default.toml`, что для таких приложений нужен `title_regex`.
- **Тест:** `only_the_scratchpad_window_of_a_two_window_app_is_shown` не зависит от выбора окна: проверяет, что второе окно не трогается.

### 3. Окно на выключенном рабочем столе не регистрируется, toggle его не видит — важная

- **Где:** `src/actor/reactor.rs:1009` (`on_windows_discovered` пропускает окна без активного space); `layout.rs:1203` (регистрация только через layout-события).
- **Чем грозит:** если scratchpad-приложение открыто на выключенном столе или при выключенном Glide, окно не попадает в реестр. Каждый toggle снова вызывает `open -b`, а спрятать окно toggle не может. Отложенный показ остаётся висеть. По коду (`register_scratchpad` → `take_pending_show`) он сработает, когда окно окажется на включённом столе, возможно через часы. Это известное упрощение из раздела 10 плана, но с выключенными столами оно проявляется заметнее.
- **Предлагается:** решение за человеком. Вариант: регистрировать scratchpad-окна независимо от того, включён ли space. Минимум — описать поведение в документации.
- **Тест:** `launched_window_on_a_disabled_space_is_left_alone` (закрепляет, что окно на выключенном столе не двигается, как требует `it_ignores_windows_on_disabled_spaces`), `window_moved_from_a_disabled_space_is_registered`.

### 4. Зарегистрированный scratchpad двигается и получает фокус даже на выключенном столе — замечание

- **Где:** `src/actor/reactor.rs:1256` (`show_scratchpad` не проверяет `screen.space`).
- **Чем грозит:** поведение непоследовательно с находкой 3: окно, зарегистрированное до выключения стола, продолжает работать, а новое не работает. Хоткеи при выключенном столе активны (`space_manager.rs:307-315`), так что сценарий реальный.
- **Предлагается:** зафиксировать в плане, что scratchpad работает и на выключенных столах (это разумно: команда явная, как `ToggleWindowFloating`), и привести к этому находку 3.
- **Тест:** `registered_scratchpad_is_shown_while_the_space_is_disabled`.

### 5. После закрытия scratchpad-окна второе окно приложения подхватывается только при следующем обновлении — замечание

- **Где:** `src/actor/layout.rs:675` (`WindowRemoved` только удаляет запись).
- **Чем грозит:** между закрытием окна и ближайшим `GetVisibleWindows` toggle вызывает `open -b`. Отложенный показ затем срабатывает на оставшемся окне при обновлении (смена стола, перезагрузка конфига), то есть позже нажатия.
- **Предлагается:** не блокирует, в рамках «одно окно на scratchpad» из скоупа. Можно учесть вместе с находкой 2.
- **Тест:** `remaining_window_of_the_app_takes_over_after_the_next_refresh`.

### 6. Двойное нажатие во время запуска вызывает `open -b` дважды — замечание

- **Где:** `src/actor/reactor.rs:1221-1226`.
- **Чем грозит:** практически ничем: повторный `open -b` для запускающегося приложения его только активирует, а окно показывается один раз (проверено).
- **Предлагается:** оставить как есть или не запускать повторно, пока отложенный показ жив.
- **Тест:** `second_toggle_before_the_window_appears_shows_it_once`. Число запусков тест не фиксирует, чтобы не мешать любому из вариантов.

### 7. Избыточная очистка `scratchpad_to_show` — замечание

- **Где:** `src/actor/layout.rs:648`, `src/actor/layout.rs:676`.
- **Чем грозит:** ничем. Мутанты M19/M20 (удаление `take_if`) эквивалентны: `take_scratchpad_to_show` и так возвращает `None` для незарегистрированного окна, а сам флаг забирается в том же `send_layout_event`.
- **Предлагается:** можно убрать при следующем касании, оставить тоже можно (защита на будущее).

## Чувствительность тестов

Ручные мутации: 37 штук в `show_scratchpad`, `set_app_hidden`, `toggle_scratchpad`, `scratchpad_window_state`, отложенном показе, регистрации и очистке реестра в `LayoutManager`, `set_config` и `validate_command`/проверке дублей в `config.rs`. Скрипт `target/r3_mutate.py` (не коммитится) вносит мутацию, прогоняет `cargo test --lib` и возвращает файл. Известный падающий тест находки 1 при подсчёте исключён.

- Первый проход: убито 30, выжило 7 (M5, M11, M19, M20, M24, M25, M32).
- На пять значимых выживших дописаны тесты (коммит `d7dce25`), повторный прогон убил их все.
- Итог: **убито 35 из 37**. M19/M20 эквивалентны (находка 7).
- Не компилировался ни один мутант. Продакшн-код после прогона не изменён: `git status` показывает только тестовые правки, `src/actor/layout.rs` возвращён через `git checkout`.

| Мутант | Результат | Кем убит (пример) |
|---|---|---|
| M1/M2 unhide всегда/никогда | убит | `toggle_shows_an_unfocused_window_without_unhiding` / `window_closed_while_hidden_is_replaced_by_a_launched_one` |
| M3 нет фокуса, M4 нет рамки | убит | 14 / 17 тестов |
| M5 рамка на первом экране, а не на активном | убит (2-й проход) | `toggle_uses_the_screen_of_the_focused_window` |
| M6 Hide шлёт `SetHidden(false)` | убит | `focused_window_on_the_other_screen_is_hidden` |
| M7 отложенный показ после ошибки запуска | убит | `failed_launch_does_not_show_the_window_later` |
| M8 `can_launch` всегда true | убит | `toggle_without_window_or_launch_only_warns` |
| M9–M13 факты состояния (focused/visible/screen/active idx/hidden) | убиты (M11 во 2-м проходе) | `focused_window_parked_off_screen_is_shown`, `toggle_shows_a_focused_window_that_is_not_on_screen`, … |
| M14/M16 отложенный показ не выполняется | убит | `second_toggle_before_the_window_appears_shows_it_once`, … |
| M15 `set_app_hidden` ничего не делает | убит | 11 тестов |
| M17/M18 реестр не чистится | убит | `destroyed_window_and_terminated_app_are_forgotten`, … |
| M19/M20 `scratchpad_to_show` не чистится | выжил, эквивалентен | — |
| M21/M22 регистрация при обновлении | убит | `renamed_rule_moves_the_window_to_the_new_name_after_reload`, … |
| M23–M25 регистрация в `WindowAdded`/`WindowSpaceChanged` | убиты (M24/M25 во 2-м проходе) | `window_moved_to_another_screen_is_registered_under_the_renamed_rule`, `window_moved_from_a_disabled_space_is_registered` |
| M26/M27 scratchpad в `toggle_focus_floating` | убит | `toggle_focus_floating_leaves_a_visible_scratchpad_for_the_tiled_windows` |
| M28 scratchpad можно затайлить | убит | `scratchpad_window_cannot_be_tiled` |
| M29 отложенный показ без `launch` | убит | `scratchpad_is_not_shown_without_launch` |
| M30 `set_config` без валидации | убит | `invalid_rules_from_ipc_are_sanitized` |
| M31 `frame` игнорируется, M32 берётся последнее правило | убит (M32 во 2-м проходе) | `first_matching_rule_decides_the_scratchpad` |
| M33–M37 `validate_command`, дубли имён | убиты | `toggle_scratchpad_with_blank_name_is_rejected`, `two_rules_with_the_same_scratchpad_name_are_rejected`, … |

Переспецификация: правка из находки 1 была временно внесена в `layout.rs` и откачена. С ней весь `cargo test --lib` зелёный (366 passed), ни один тест править не пришлось. Тесты находок 2 и 6 намеренно не фиксируют спорные детали: какое окно выбрано и сколько было запусков.

Инструмент mutation-тестирования (`cargo-mutants`) в проекте не настроен и не ставился.

## Допущения

- Проверка через стенд Reactor: `WindowServer`/`SpaceManager` не запущены. Перезагрузка конфига моделируется как `ConfigChanged` + `SpaceChanged` + ответы на `GetVisibleWindows`: по коду `space_manager.rs` (`ConfigUpdated` → `request_space_refresh`) так и происходит, но вживую это не проверено.
- «Glide выключен глобально» и «выключен стол» на уровне Reactor неразличимы (`screen.space == None`), поэтому проверяется один случай. Ожидание для зарегистрированного окна (показывать) взято по аналогии с `ToggleWindowFloating`, которая тоже работает на выключенных столах. Тест `registered_scratchpad_is_shown_while_the_space_is_disabled` закрепляет это поведение. Если решение будет другим, тест нужно поменять вместе с кодом.
- Находку 1 я считаю дефектом, а не расширением скоупа: CLAUDE.md требует, чтобы при перезагрузке изменённые значения применялись. Тест оставлен красным, без `#[ignore]`.
- Найденная нестабильность выбора окна (находка 2) детерминирована для FxHash, поэтому тесты не мигают. Они написаны так, чтобы не зависеть от выбора.
- Сценарий «приложение упало до появления окна» (отложенный показ живёт вечно) не тестировался: это известное упрощение из раздела 10.

## Было сломано до начала работы

Нет: базовый прогон 345/345.
