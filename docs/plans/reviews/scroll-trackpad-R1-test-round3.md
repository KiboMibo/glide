# Тесты: волна 1 плана scroll-trackpad, круг 3 (после F2): PASS_WITH_FINDINGS

- **Дата:** 2026-09-21
- **Статус:** PASS_WITH_FINDINGS
- **Ветка:** `test/R1-scroll-round3` (от `fix/F2-scroll`, `7bac1c5`)
- **Скоуп:** `git diff 492af82..fix/F2-scroll -- src glide.default.toml`. Это `SwipeGesture` и `dominant_axis` в `src/model/scroll_viewport.rs`, `handle_scroll_wheel` в `src/actor/layout.rs` и передача `(delta_x, delta_y)` в `src/actor/reactor.rs`. Отдельно проверены ~25 тестов, у которых F2 поменяла ожидания (список в `docs/reports/F2-scroll-2026-09-21.md`).
- **Команда прогона:** `cargo test`
- **Прошлые отчёты:** `scroll-trackpad-R1-test.md`, `scroll-trackpad-R1-test-round2.md` (не менялись)

## Итог

Каждый критерий F2 закреплён тестом, который падает, если сломать соответствующую логику: 29 мутантов, все убиты. После исходного набора F2 выжили два мутанта (M02: ось по текущему событию, M23: нулевая дельта колеса листает). Под них я добавил тесты. Тесты с изменёнными ожиданиями по числам не ослаблены. По смыслу ослаблена одна проверка: инерция в Reactor-тесте после `Ended` больше ничего не проверяет. Её закрывает новый тест (находка 1).

Добавлено 15 тестов: 14 в `actor::layout::tests::r1_scroll_wheel::f2::edges` и 1 в `actor::reactor::tests::scroll_wheel::r1`. Весь `cargo test` зелёный: 565 lib + 7 интеграционных (до начала работы было 550 + 7). Прогон повторён дважды, результат стабильный. `cargo build --all-targets` без warnings. Продакшн-код не менялся: `git diff fix/F2-scroll HEAD` затрагивает только `#[cfg(test)]`-модули.

Базовый прогон до начала работы: 550 + 7, всё зелёное.

## Критерии F2 → тесты → мутанты

| Критерий | Тесты | Мутанты, которые они убивают |
|----------|-------|------------------------------|
| Одна колонка на жест | `trackpad_gesture_moves_one_column_once_the_swipe_threshold_is_reached`, `f2::horizontal_swipes_…`, `f2::vertical_swipes_…`, `edges::first_gesture_without_began_still_moves_only_one_column`, `edges::single_huge_event_moves_exactly_one_column`, модельный `swipe_moves_once_per_gesture_…` | M01 (нет `done` после колонки): 17 тестов |
| Ось по суммарному пути | `f2::the_axis_is_chosen_by_total_travel_not_by_the_event`, `edges::vertical_gesture_with_horizontal_jitter_moves_along_y` (новый), `edges::axis_switches_when_the_other_axis_overtakes_mid_gesture` (новый), модельный `swipe_axis_follows_the_total_travel` | M02 (ось по событию): только новый тест с дрожанием dx; M04 (побеждает ось, первой дошедшая до порога): новый `axis_switches_…`; M22 (знак всегда по x): 9 тестов; M29 (разворот прибавляет путь, а не вычитает): 3 |
| Порог 40 × 20 / s | `f2::swipe_shorter_than_40pt_does_not_scroll`, `f2::sensitivity_40_halves_the_swipe_and_0_disables_it`, `r2::trackpad_swipe_per_column_is_40pt_times_20_over_sensitivity`, Reactor `r2::huge_sensitivity_from_config_scrolls_one_column_per_8pt` | M05 (40 → 250): 45; M06 (`>=` → `>`): 36; M07 (чувствительность не учитывается): 6; M30 (порог жеста = шаг колеса): 44 |
| Сброс на `Began`, в т.ч. `MayBegin`+`Began` | `f2::may_begin_then_began_after_movement_starts_a_fresh_gesture`, `r2::gesture_start_without_prior_end_resets_progress`, `began_with_movement_resets_progress_before_counting_its_delta`, `edges::two_gestures_without_ended_move_one_column_each` (новый), `mouse::tests::scroll_phase_maps_cg_values` | M08 (нет `begin`): 22; M20 (`begin` не сбрасывает `done`): 20; M21 (`begin` не сбрасывает путь): 19; M28 (`begin` после учёта дельты): 2; M19 (`MayBegin` не мапится в `Began`): 1 |
| `Ended` закрывает жест, его дельта учитывается | `trackpad_gesture_end_discards_leftover_progress`, `r2::gesture_end_counts_its_delta_before_discarding_leftover`, `edges::changed_after_ended_without_began_does_not_scroll`, `edges::ended_without_began_counts_its_delta_only_in_an_open_gesture` | M09 (нет `end`): 4; M10 (`end` до учёта дельты): 3 |
| Инерция игнорируется | `trackpad_momentum_neither_scrolls_nor_accumulates`, `momentum_is_ignored_in_both_directions_…`, `edges::momentum_inside_an_open_gesture_is_ignored` (новый), Reactor `momentum_inside_an_open_gesture_neither_scrolls_nor_counts` (новый) | M11 (инерция учитывается): 4 |
| Continuous без фаз: пропорционально, ось по событию | `f2::continuous_scroll_without_phases_moves_one_column_per_40pt`, `…_uses_the_dominant_axis_of_each_event`, `edges::vertical_continuous_scroll_without_phases_is_proportional` (новый), `huge_trackpad_delta_moves_at_most_16_columns` | M12 (без фаз как жест): 5; M13 (без фаз только dx): 5; M26 (предел 16 → 17): 2 |
| Колесо: доминирующая ось, сброс остатка, ноль не листает | `f2::wheel_notch_on_either_axis_moves_one_column`, `f2::wheel_notch_ignores_leftover_continuous_progress`, `edges::wheel_notch_at_45_degrees_uses_the_horizontal_axis`, `edges::zero_wheel_delta_does_not_scroll` (новый), Reactor `vertical_wheel_notch_with_modifier_scrolls` | M14 (колесо только dx): 4; M15 (без сброса остатка): 1; M23 (нулевая дельта листает): только новый тест; M03 (ничья → y): 3 |
| `invert_scroll_direction` для всех видов | `f2::invert_scroll_direction_flips_every_kind_of_event`, `edges::invert_scroll_direction_flips_vertical_continuous_and_wheel` (новый), `trackpad_scroll_respects_invert_direction`, `trackpad_invert_direction_applies_to_partial_progress`, `wheel_invert_direction_flips_notches` | M16 (инверсия только по x): 2; M25 (инверсии нет): 5 |
| Reactor передаёт обе дельты и фазы без модификатора | `vertical_trackpad_swipe_with_horizontal_noise_uses_delta_y`, `vertical_trackpad_swipe_falls_back_to_delta_y`, `gesture_phases_without_modifier_reset_progress` | M17 (Reactor теряет dy): 3; M18 (фазы без модификатора отбрасываются): 1 |
| Знак вертикали (dy < 0 → следующая колонка) | `f2::vertical_swipes_…`, `f2::wheel_notch_on_either_axis_…`, Reactor-тесты вертикали | M24 (знак dy перевёрнут): 15 |

## Мутации

Мутанты вносились локально через `perl -0pi` (скрипты лежат в `target/mut/`, в git не попадают). После каждой мутации запускался `cargo test --lib`, затем файл восстанавливался через `git checkout`. В конце `git status` и `git diff fix/F2-scroll HEAD` по продакшн-файлам пустые.

| # | Мутация | Файл | Убит |
|---|---------|------|------|
| M01 | `add` не ставит `done` после колонки | scroll_viewport.rs | 17 |
| M02 | ось выбирается по текущему событию | scroll_viewport.rs | 1 (новый тест; до него выжил) |
| M03 | ничья `\|dx\|==\|dy\|` → y | scroll_viewport.rs | 3 |
| M04 | побеждает ось, первой дошедшая до порога | scroll_viewport.rs | 1 (новый тест) |
| M05 | `TRACKPAD_SWIPE_PT` 40 → 250 | layout.rs | 45 |
| M06 | `>=` порога → `>` | scroll_viewport.rs | 36 |
| M07 | `scale = 1` (чувствительность не учитывается) | layout.rs | 6 |
| M08 | нет `swipe.begin()` на `Began` | layout.rs | 22 |
| M09 | нет `swipe.end()` на `Ended` | layout.rs | 4 |
| M10 | `end()` до учёта дельты `Ended` | layout.rs | 3 |
| M11 | инерция не отбрасывается | layout.rs | 4 (с новым Reactor-тестом) |
| M12 | continuous без фаз идёт через жест | layout.rs | 5 |
| M13 | continuous без фаз берёт только dx | layout.rs | 5 |
| M14 | колесо берёт только dx | layout.rs | 4 |
| M15 | деление колеса не сбрасывает остаток | layout.rs | 1 |
| M16 | `invert` только по x | layout.rs | 2 |
| M17 | Reactor передаёт `(dx, 0)` | reactor.rs | 3 |
| M18 | Reactor отбрасывает `Began`/`Ended` без модификатора | reactor.rs | 1 |
| M19 | `MayBegin` (128) не мапится в `Began` | mouse.rs | 1 |
| M20 | `begin` не сбрасывает `done` | scroll_viewport.rs | 20 |
| M21 | `begin` не сбрасывает путь | scroll_viewport.rs | 19 |
| M22 | знак колонки всегда по `travel_x` | scroll_viewport.rs | 9 |
| M23 | нулевая дельта колеса листает (`0.0.signum() = 1`) | layout.rs | 1 (новый тест; до него выжил) |
| M24 | знак dy перевёрнут | layout.rs | 15 |
| M25 | `invert` не работает совсем | layout.rs | 5 |
| M26 | предел шагов 16 → 17 | layout.rs | 2 |
| M28 | `begin` после учёта дельты `Began` | layout.rs | 2 |
| M29 | разворот по x прибавляет путь | scroll_viewport.rs | 3 |
| M30 | порог жеста = шаг колеса `W / min(n, 3)` | layout.rs | 44 |

Итого 29 мутантов, все убиты. Номер M27 пропущен. `cargo-mutants` в проекте не настроен, полноценный mutation-прогон не проводился.

## Тесты с изменёнными ожиданиями

Вывод: по числам ни один тест не ослаблен. Каждый по-прежнему падает на мутантах, которые ломают его свойство.

| Тест | Свойство | Падает на | Вывод |
|------|----------|-----------|-------|
| `trackpad_gesture_moves_one_column_once_the_swipe_threshold_is_reached` (было `…_carries_remainder`) | колонка на 40 pt, дальше в жесте ничего | M01, M05, M06, M08, M20, M21, M30 | смысл изменён по решению пользователя; «одна колонка на жест» проверяет строже, чем старый тест проверял перенос |
| `trackpad_scroll_threshold_scales_with_sensitivity` | при 40 порог 20 pt | M05, M06, M07, M30 | не ослаблен |
| `trackpad_momentum_neither_scrolls_nor_accumulates` | инерция внутри жеста не копится | M11, M05, M06 | не ослаблен |
| `trackpad_gesture_start_resets_progress` | `Began` сбрасывает | M08, M20, M21, M05 | усилен: в круге 2 не ловил отсутствие сброса (находка 1 круга 2), теперь ловит |
| `trackpad_threshold_does_not_depend_on_screen_width` | порог не от ширины | M05, M06, M30 | не ослаблен |
| `trackpad_gesture_end_discards_leftover_progress` | после `Ended` не листается | M09 | не ослаблен: −100 pt после `Ended` больше порога, значит тест ловит «`Ended` ничего не делает» |
| `trackpad_scroll_respects_invert_direction` | инверсия | M25, M08, M20, M21 | не ослаблен |
| `zero_sensitivity_trackpad_never_scrolls_and_keeps_progress_finite` | 0 не листает | M07, M05, M06 | не ослаблен |
| `huge_trackpad_delta_moves_at_most_16_columns` | предел 16 шагов | M26, M12 | не ослаблен; для жеста с фазами огромную дельту (ровно 1 колонка) закрепляет новый `single_huge_event_moves_exactly_one_column` |
| `zero_delta_phase_events_do_not_scroll` | нулевые дельты не листают | M08, M20, M05, M06 | не ослаблен; добавленный `Began` нужен, потому что `Ended` закрывает жест |
| `began_with_movement_resets_progress_before_counting_its_delta` | порядок «сброс, затем дельта» | M08, M21, M28 | не ослаблен |
| `reversing_direction_within_a_gesture_cancels_progress` | разворот вычитает путь | M29, M05, M06 | не ослаблен |
| `trackpad_invert_direction_applies_to_partial_progress` | инверсия неполного пути | M25, M08, M20, M21 | не ослаблен |
| `non_finite_deltas_do_not_panic_and_next_gesture_recovers` | NaN/∞ не ломают следующий жест | M08, M20, M21 | не ослаблен; для NaN/∞ по y добавлен `non_finite_vertical_delta_is_dropped_by_the_next_began` |
| `switching_space_mid_gesture_does_not_carry_progress` | путь не переходит на другой стол | M05, M06, M30 | не ослаблен: общий на все столы путь дал бы 60 ≥ 40 на втором событии, а тест ждёт `o(1)` |
| `layout_kind_change_mid_gesture_is_safe` | смена раскладки посреди жеста | M05, M06, M30 | не ослаблен |
| `two_columns_use_the_same_trackpad_threshold` | порог не зависит от числа колонок | M05, M06, M30 | не ослаблен |
| `r2::gesture_end_counts_its_delta_before_discarding_leftover` | дельта `Ended` считается | M09, M10, M21 | не ослаблен |
| `r2::trackpad_swipe_per_column_is_40pt_times_20_over_sensitivity` | формула порога | M07, M05, M06 | не ослаблен |
| `r2::nan_sensitivity_in_config_uses_default_trackpad_threshold` | NaN в конфиге → 20 | M05, M06 | не ослаблен (ветку NaN в `validated()` ловят тесты круга 2) |
| `r2::gesture_start_without_prior_end_resets_progress` | `Began` без `Ended` | M08, M21 | не ослаблен |
| `r2::partial_reversal_subtracts_from_progress` | частичный разворот | M29, M05, M06 | не ослаблен |
| Reactor `trackpad_gesture_scrolls_one_column_and_ignores_momentum` | колонка, затем инерция | M01, M05, M30; **не падает на M11** | часть про инерцию ослаблена по смыслу, см. находку 1; закрыто новым тестом |
| Reactor `gesture_phases_without_modifier_reset_progress` | `Began`/`Ended` без модификатора открывают и закрывают жест | M18, M09, M08, M20, M21 | не ослаблен; второй сценарий переписан, но ловит отбрасывание `Began` без модификатора |
| Reactor `r2::gesture_phase_events_with_modifier_keep_their_delta` | дельта `Began`/`Ended` с модификатором | M10, M08, M20 | не ослаблен |
| Reactor `r2::huge_sensitivity_from_config_scrolls_one_column_per_8pt` | clamp до 100 → 8 pt | M07, M05, M06 | не ослаблен |
| Reactor `r2::nan_sensitivity_from_config_uses_default_for_trackpad` | NaN → 20 | M05, M06 | не ослаблен |
| Reactor `huge_sensitivity_from_config_is_clamped_for_trackpad` | без clamp −1 pt пролистнул бы колонку | не менялся (только комментарий) | не ослаблен |

## Краевые случаи (новые тесты, `f2::edges`)

| Случай | Тест | Поведение |
|--------|------|-----------|
| Жест без `Began`, первое событие `Changed` | `first_gesture_without_began_still_moves_only_one_column` | на новом viewport листает одну колонку, дальше ничего до `Ended` |
| `Changed` после `Ended` без нового `Began` | `changed_after_ended_without_began_does_not_scroll` | не листает; следующий `Began` открывает жест |
| `Ended` без `Began` | `ended_without_began_counts_its_delta_only_in_an_open_gesture` | в открытом жесте дельта `Ended` учитывается; второй `Ended` ничего не делает |
| Диагональ 45° (`\|Σdx\| == \|Σdy\|`) | `diagonal_swipe_at_45_degrees_uses_the_horizontal_axis`, `wheel_notch_at_45_degrees_…` | побеждает x (и у жеста, и у колеса) |
| Ось меняется посреди жеста | `axis_switches_when_the_other_axis_overtakes_mid_gesture` | сначала dx 35, потом dy −45 → y; Σ(41, −45) → y, хотя x перешёл порог в том же событии |
| dx каждого события больше, но Σ по y больше | `vertical_gesture_with_horizontal_jitter_moves_along_y` | y |
| Один огромный `Changed` / `Began` | `single_huge_event_moves_exactly_one_column` | ровно 1 колонка, инерция 1e7 ничего не делает |
| Два жеста подряд без `Ended` (и двойной `Began`) | `two_gestures_without_ended_move_one_column_each` | по колонке на жест |
| Инерция внутри открытого жеста | `momentum_inside_an_open_gesture_is_ignored` + Reactor-тест | не листает и не копится |
| NaN/±∞ по y | `non_finite_vertical_delta_is_dropped_by_the_next_began` | без паники; `Began` восстанавливает |
| Вертикаль без фаз | `vertical_continuous_scroll_without_phases_is_proportional` | −80 → 2 колонки, остаток переносится |
| Нулевая дельта колеса, в т.ч. `-0.0` | `zero_wheel_delta_does_not_scroll` | не листает |
| `invert` для вертикали: колесо, без фаз, жест | `invert_scroll_direction_flips_vertical_continuous_and_wheel` | все три инвертируются |

## Находки

1. **Замечание** (закрыто тестом). `src/actor/reactor.rs:4153-4157`, `trackpad_gesture_scrolls_one_column_and_ignores_momentum`; то же в `f2::horizontal_swipes_…`/`vertical_swipes_…` через хелпер `gesture` (`src/actor/layout.rs:4246`). Инерция идёт после `Ended`, а `Ended` закрывает жест, поэтому эти проверки проходят и без ветки `phase == Momentum` (мутант M11 их не роняет). Критерий «инерция — 0 колонок» остаётся верным, но в Reactor инерцию больше ничего не проверяло. Добавлен `momentum_inside_an_open_gesture_neither_scrolls_nor_counts`, он M11 ловит.
2. **Замечание** (поведение, не нарушает критерии). `src/model/scroll_viewport.rs:35-38`, `src/actor/layout.rs:1484-1499`. Состояние жеста хранится отдельно в каждом `ViewportState`, то есть на раскладку. `SwipeGesture::default()` открыт (`done = false`), а после `Ended` закрыт. Поэтому `Changed` без `Began` листает на столе, где жестов ещё не было, и не листает там, где предыдущий жест закончился. Если стол под курсором сменится посреди жеста, остаток жеста может пролистнуть ещё одну колонку на новом столе, а может и не пролистнуть, в зависимости от истории этого стола. Оба варианта закреплены тестами (`first_gesture_without_began_…`, `changed_after_ended_without_began_…`). Если нужна предсказуемость, можно создавать `SwipeGesture` закрытым, чтобы листал только жест, начатый с `Began`. Это решение для F-задачи и пользователя, не для тестов.
3. **Замечание** (из отчёта F2, тестами не решается). Направление вертикального свайпа (пальцы вверх при естественной прокрутке → следующая колонка) выбрано по конвенции. Тесты закрепляют эту конвенцию (M24 убивают 15 тестов), но правильна ли она, покажет только живая проверка.

Блокирующих и важных находок нет.

## Допущения

- Вызов делегированный. Отчёт положен рядом с отчётами кругов 1–2. JSON-сводку, как и в круге 2, не делал, сводка есть в финальном сообщении вызвавшему агенту.
- «Ось меняется посреди жеста» и «жест без `Began`» проверены на текущем поведении, которое описано в отчёте F2 как задуманное.
- Покрытие по строкам не считалось (`cargo-llvm-cov` в проекте не настроен). Вместо него чувствительность подтверждена мутациями по каждой ветке `handle_scroll_wheel` и `SwipeGesture`: ни одной ветки из diff без убитого мутанта нет.

## Коммиты

- `1d689c0` test: Cover F2 swipe edge cases: no Began, 45 degrees, axis switch, huge delta
- `157155e` test: Pin zero wheel delta and total-travel axis under horizontal jitter
- `1d9a7f2` test: Check through the Reactor that momentum inside a gesture is ignored
- отчёт (этот файл)
