# Тесты: волна 3 scratchpad, второй круг (после F3): PASS_WITH_FINDINGS

- **Дата:** 2026-09-16
- **Статус:** PASS_WITH_FINDINGS
- **Скоуп:** поведение, которое F3 добавила или изменила (`git diff eba9883^1 eba9883 -- src`): `src/model/scratchpad.rs`, `src/actor/layout.rs`, `src/actor/reactor.rs`, `src/actor/reactor/main_window.rs`, `src/actor/window_server.rs`, `src/sys/app.rs`. Отдельно проверено, закрыты ли находки 1 и 2 первого круга (`scratchpad-R3-test.md`).
- **Ветка:** `test/R3-tests-round2` (от `wave/scratchpad-w3`, в ней уже смержена F3). Коммиты: `e7b08ad`, `03c65ea`, `9512ab1`.
- **Команда прогона:** `cargo test --no-fail-fast`

## Итог

Находки 1 и 2 первого круга закрыты. Тест `removed_rule_stops_the_window_being_a_scratchpad_after_reload` теперь зелёный, его ожидания не менялись. Выбор окна больше не зависит от порядка в хеш-множестве, это подтверждают тесты F3 и три мутации. Находка 3 (выключенные столы) по решению командира перенесена в T6, здесь она не считается.

Добавлено 12 тестов: 10 в стенд Reactor (`edge_cases`) и 2 в `window_server`. Весь `cargo test` зелёный: lib 402/402, doc 7/7, warnings нет. Прогнано 28 ручных мутаций кода F3: 23 убиты, 5 выживших эквивалентны. Новая важная находка одна: главное окно не выбирается, если оно на другом экране, а `glide.default.toml` обещает именно главное окно. Блокирующих находок нет.

## Статус находок первого круга

| # | Находка | Статус | Чем подтверждено |
|---|---------|--------|------------------|
| 1 | Удалённое правило не снимает с окна статус scratchpad | **закрыта** | `edge_cases::removed_rule_stops_the_window_being_a_scratchpad_after_reload` зелёный. Новые тесты: `rule_moved_to_another_app_releases_the_old_window` (правило осталось, но теперь относится к другому приложению), `pending_show_of_a_renamed_rule_is_dropped`, `pending_show_survives_a_reload_that_keeps_the_rule`. Мутанты N13 и N14 убиты |
| 2 | Окно выбирается в порядке FxHashSet | **закрыта** в пределах одного экрана | Мутанты N10–N12 и N28 убиты тестами `main_window_of_a_two_window_app_becomes_the_scratchpad` и `lowest_window_becomes_the_scratchpad_without_a_main_window`. Если окна приложения на разных экранах, выбор тоже детерминирован, но главное окно не предпочитается (новая находка A) |
| 3 | Окна на выключенных столах | перенесена в T6 | не проверялась |

## Что покрыто (новые тесты)

| Поведение F3 | Тесты |
|---|---|
| Срок жизни отложенного показа | `expiry_of_an_unknown_show_changes_nothing` (истечение уже снятого показа ничего не делает, повторное тоже), `toggle_after_an_expired_show_launches_and_shows_again` (новый id, новый запуск, окно показывается) |
| Отмена показа при завершении приложения | `quitting_another_app_keeps_the_pending_show`, `app_quitting_before_its_window_appears_is_not_shown_on_relaunch` |
| Истечение старого запуска не отменяет новый; запись и воспроизведение | `launch_ids_are_the_same_on_replay`: после воспроизведения трассы с двумя запусками одного имени и истечением первого отложенные показы совпадают с исходными |
| Несколько показов за одно событие (`handle_layout_response_with_context`) | `every_scratchpad_found_by_one_launch_is_shown_at_once`: одно `ApplicationLaunched` регистрирует два scratchpad, оба сразу получают рамку и фокус в порядке регистрации |
| `retain_names` и переименование правил | `rule_moved_to_another_app_releases_the_old_window`, `pending_show_of_a_renamed_rule_is_dropped`, `pending_show_survives_a_reload_that_keeps_the_rule` |
| Скрытие приложения системой обновляет видимые окна | `app_hidden_by_the_system_updates_the_next_toggle` (Cmd+H на самом scratchpad-приложении, затем toggle показывает окно) |
| `RecheckVisibleWindows` и `ApplicationHiddenChanged` в WindowServer | `hidden_change_and_recheck_report_a_partial_update_for_the_app` (обе проверки шлют `pid: Some(pid)`, а не полное обновление), `hidden_change_without_a_window_change_sends_only_the_event` |

Уже существующие тесты F3 (`window_appearing_after_the_show_expired_is_not_shown`, `expiry_of_an_earlier_launch_keeps_a_later_one`, `quitting_the_launched_app_drops_the_pending_show`, `show_expiry_is_recorded_and_replayed`, `show_queued_outside_send_layout_event_is_taken_on_screen_change`, четыре теста `window_server` и тесты модели) проверены мутациями и оставлены без изменений.

Коммит `9512ab1` исправляет две мои проверки из `e7b08ad`. `Test::settle()` вычищает `raise_rx`, поэтому `focus_requests()` после `settle()` пуст всегда. Проверки перенесены до `settle()`, и тесты стали убивать N4 и N13.

## Результат прогона

- Базовый прогон (до работы, `cargo test --no-fail-fast`): lib 390 passed, 0 failed; doc 7 passed.
- Итоговый прогон: lib **402 passed, 0 failed**; doc 7 passed; warnings нет.
- Стабильность: `cargo test --lib -- scratchpad window_server` 3 раза подряд и 1 раз с `--test-threads=1`, каждый раз 139/139.
- Формат: nightly `rustfmt --edition 2024 --check` по `src/actor/reactor.rs` и `src/actor/window_server.rs` проходит чисто.

## Покрытие

- `cargo llvm-cov` и `cargo mutants` не установлены. Покрытие оценено вручную по исполняемым строкам diff F3 и подтверждено мутациями: **~95%** изменённых строк при пороге 80%.
- Не исполняются:
  - `src/actor/reactor.rs:398-409`: настоящий launcher в `Reactor::spawn` (ожидание 30 с, отправка `ScratchpadShowExpired`). Для теста нужен запуск `open`, это запрещено. См. находку B.
  - Тело потока в `launch_app_then` (`src/sys/app.rs`): тот же настоящий `open`.
  - Запасное значение `unwrap_or_else` в `Reactor::toggle_scratchpad`: недостижимо по построению.
  - Ветка `warn!` в `Launch`, когда `launch` задан, а отложенного показа нет: недостижима, потому что `toggle` с `Some(launch)` всегда создаёт показ.

## Находки

### A. Главное окно не выбирается, если оно на другом экране (важная)

- **Где:** `src/actor/reactor.rs:1046` (сортировка) и `:1058` (по одному `WindowsOnScreenUpdated` на каждый экран); `src/actor/layout.rs:1227` (занятое имя не перерегистрируется, выигрывает первое окно). Обещание дано в `glide.default.toml:262`: «the app's main window if it matches».
- **Как воспроизвести:** временный тест, в репозиторий не добавлен. Два экрана, у приложения 3 окна `(3,1)` на первом экране и `(3,2)` на втором, главное окно `(3,2)`. Окно `(3,1)` приходит на первый экран первым событием, поэтому scratchpad `"l"` становится `(3,1)`.
- **Чем грозит:** выбор детерминирован, но расходится с документацией. Если окна у приложения появляются по одному (`WindowCreated` → `WindowBecameVisible`), тоже выигрывает первое появившееся окно, а не главное. Основной сценарий с одним окном это не затрагивает.
- **Предлагается:** либо ослабить формулировку в `glide.default.toml`, например «first matching window; the main window is preferred when the app reports several at once», либо сортировать окна до разбиения по экранам. Красный тест не оставлен, см. «Допущения».

### B. Настоящий launcher из `Reactor::spawn` не покрыт тестами (замечание)

- **Где:** `src/actor/reactor.rs:398-409`.
- **Чем грозит:** в замыкании есть логика: ждать только при успешном `open`, отправлять событие всегда, считать срок от момента нажатия. Ни один тест её не исполняет. Например, `if !launched` сломает срок жизни незаметно для тестов.
- **Предлагается:** вынести замыкание в функцию, которая принимает функцию запуска, `Sender` и задержку, и проверить её с фиктивным запуском.

### C. Эквивалентные мутанты: защитный код очереди показов (замечание)

- **Где:** `src/actor/layout.rs:652`, `:680` (`retain` очереди на `AppClosed`/`WindowRemoved`), `:1210` (пропуск окна без рамки в `take_scratchpad_to_show`); `src/actor/reactor.rs:1161-1163` (забор всей очереди до `apply_layout_response`).
- **Чем грозит:** ничем. Очередь забирается после каждого `handle_event`, а `show_scratchpad` сам вызывает `handle_layout_response` и рекурсивно забирает следующее окно. Поэтому эти строки не влияют на наблюдаемое поведение (мутанты N8, N9, N16, N18, N19).
- **Предлагается:** оставить как защиту или упростить при следующем касании.

### D. Тест `launch_app_then_does_not_call_back_for_an_invalid_id` небезопасен при регрессии (замечание)

- **Где:** `src/sys/app.rs:410`.
- **Чем грозит:** если проверку id сломать, тест запустит настоящий `/usr/bin/open -b` с этими id. `panic!` в колбэке идёт в чужом потоке и тест не роняет. Сейчас тест падает только на `assert!(result.is_err())`, так что защищает он не колбэк, а валидацию.
- **Предлагается:** проверять `validate_bundle_id` напрямую или ввести шов, через который тест подменяет запуск `open`.

### E. `hidden_change_schedules_a_recheck` зависит от реального времени (замечание)

- **Где:** `src/actor/window_server.rs:632`.
- **Чем грозит:** тест ждёт настоящий поток с `sleep(250 ms)`, запас 5 с. За 4 прогона не мигнул, но на перегруженном CI риск есть.
- **Предлагается:** оставить или передавать задержку параметром.

## Чувствительность тестов

Ручные мутации (скрипт `target/r3r2_mutate.py`, не коммитится) вносятся по одной, после каждой запускается `cargo test --lib --no-fail-fast`, затем файл возвращается к исходному содержимому. После прогона `git status` показывает только `.omc/`, продакшн-код не изменён. Ни один мутант не сломал компиляцию. Мутанты не трогают настоящий запуск приложений.

| Мутант | Результат | Кем убит (пример) |
|---|---|---|
| N1 `ScratchpadShowExpired` ничего не делает | убит | `window_appearing_after_the_show_expired_is_not_shown`, `expiry_of_an_unknown_show_changes_nothing` |
| N2 `cancel_pending_show` не сверяет id | убит | `expiry_of_an_earlier_launch_keeps_a_later_one`, `launch_ids_are_the_same_on_replay` |
| N3 id не растёт | убит | `toggle_after_an_expired_show_launches_and_shows_again` и ещё 4 |
| N4 завершение приложения не снимает показ | убит | `quitting_the_launched_app_drops_the_pending_show`, `app_quitting_before_its_window_appears_is_not_shown_on_relaunch` |
| N5 сравнение bundle id с учётом регистра | убит | `closed_app_cancels_only_its_pending_shows` |
| N6 завершение любого приложения снимает все показы | убит | `quitting_another_app_keeps_the_pending_show` и ещё 3 |
| N7 ошибка launcher не снимает показ | убит | `failed_launch_does_not_show_the_window_later` |
| N8 забирать по одному показу за ответ | выжил, эквивалентен | — (находка C) |
| N9 забирать показы после `apply_layout_response` | выжил, эквивалентен | — (находка C) |
| N10 без сортировки / N11 сортировка без главного окна / N12 главное окно последним | убиты | `main_window_of_a_two_window_app_becomes_the_scratchpad`, `lowest_window_becomes_the_scratchpad_without_a_main_window` |
| N13 `set_config` не вызывает `retain_names` | убит | `reload_without_the_rule_forgets_the_scratchpad`, `pending_show_of_a_renamed_rule_is_dropped` |
| N14 `register_scratchpad` не снимает регистрацию | убит | `rule_moved_to_another_app_releases_the_old_window` (новый) |
| N15 регистрация в обратном порядке | убит | `every_scratchpad_found_by_one_launch_is_shown_at_once` и ещё 4 |
| N16 окно без рамки останавливает забор | выжил, эквивалентен | — |
| N17 очередь забирается с конца | убит | `every_scratchpad_found_by_one_launch_is_shown_at_once`, `scratchpads_registered_by_one_event_are_all_shown` |
| N18 / N19 очередь не чистится на `AppClosed` / `WindowRemoved` | выжили, эквивалентны | — |
| N20 toggle без `launch` оставляет старый показ | убит | `toggle_without_launch_leaves_no_pending_show` |
| N21 повторная проверка не планируется | убит | `hidden_change_schedules_a_recheck` |
| N22 нет немедленной проверки | убит | `hiding_an_app_updates_the_visible_windows` и ещё 2 |
| N23 обновление окон уходит раньше события | убит | `hiding_an_app_updates_the_visible_windows` |
| N24 `RecheckVisibleWindows` ничего не делает | убит | `recheck_sends_the_visible_windows_only_if_they_changed` |
| N25 / N26 повторная или немедленная проверка шлёт полное обновление (`pid: None`) | убиты | `hidden_change_and_recheck_report_a_partial_update_for_the_app` (новый; до него N25 выживал) |
| N27 любое событие Reactor запускает проверку | убит | `other_reactor_events_do_not_update_the_visible_windows` |
| N28 `app_main_window` всегда `None` | убит | `main_window_of_a_two_window_app_becomes_the_scratchpad` |

Итог: **убито 23 из 28**, 5 выживших эквивалентны. Переспецификации нет: новые тесты не проверяют текст логов, число запусков (кроме явного повторного запуска после истечения) и выбор окна между экранами.

## Допущения

- Находку A я не оставил красным тестом, по двум причинам. Командир требует зелёный `cargo test`. Кроме того, первый круг фиксировал недетерминированность выбора, а она устранена. Какой вариант правильный, поправить документацию или сортировку, решает человек. Воспроизведение описано в находке.
- Вызов делегированный, поэтому тесты добавлены в существующий модуль `tests::scratchpad::edge_cases` в `src/actor/reactor.rs`: помощники стенда (`with_screens`, `refresh`, `frames_of`) приватны в этом модуле. `testing.rs` не менялся.
- «Отмена по ошибке `open`» проверена на уровне Reactor через launcher, который возвращает ошибку (`failed_launch_does_not_show_the_window_later`). Асинхронная ошибка `open` приходит тем же событием `ScratchpadShowExpired`, и этот путь покрыт тестами N1–N3. Сам настоящий launcher не исполнялся (находка B).
- Воспроизведение проверено функциями `replay::tests::recorded_trace`/`replay_trace`. Живые трассы не записывались: запуск WM запрещён.

## Было сломано до начала работы

Нет: базовый прогон 390/390, doc 7/7.
