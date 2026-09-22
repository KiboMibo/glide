# Тесты: волна 1 плана scroll-trackpad, круг 2 (после F1) — PASS_WITH_FINDINGS

- **Дата:** 2026-09-21
- **Статус:** PASS_WITH_FINDINGS
- **Ветка:** `test/R1-scroll-round2` (от `wave/scroll-trackpad-w1`, `929065a`: T1 + R1-test + R1-qa + F1)
- **Скоуп:** правки F1 (`docs/reports/F1-scroll-2026-09-21.md`) в `src/actor/layout.rs` (`handle_scroll_wheel`, `TRACKPAD_COLUMN_SWIPE_PT`), `src/actor/reactor.rs` (`Event::ScrollWheel`, фазы без модификатора), `src/config.rs` (`ScrollConfig::validated`), и 14 тестов, у которых F1 поменяла ожидания
- **Команда прогона:** `cargo test --no-fail-fast`
- **Отчёт круга 1:** `docs/plans/reviews/scroll-trackpad-R1-test.md` (не менялся)

## Итог

Находки круга 1 (1, 2, 3, 5) и R1-qa (1, 3) закрыты, это подтверждают тесты и мутации. Ни один из 14 тестов с новыми ожиданиями не ослаблен по числам: каждый по-прежнему падает, если сломать логику, которую он проверяет. Есть одно исключение по смыслу: `trackpad_gesture_start_resets_progress` больше не ловит отсутствие сброса на `Began`, потому что теперь прогресс сбрасывает стоящий перед ним `Ended` (находка 1, замечание). Для этого случая я добавил отдельный тест.

Добавлено 9 тестов. После них живых неэквивалентных мутантов нет: 28 мутантов, убито 27, один эквивалентный. Весь `cargo test` зелёный: 531 lib + 7 интеграционных.

## Статус находок круга 1

| Находка | Статус | Чем подтверждено |
|---------|--------|------------------|
| R1-test 1 / R1-qa 3: остаток после неполного свайпа глотает деление колеса | закрыта | `mouse_wheel_notch_after_partial_trackpad_swipe_moves_one_column` зелёный; мутант M01 (нет сброса на `Ended`) убивают 4 теста; M02 (сброс до учёта дельты `Ended`) убивают новые `gesture_end_counts_its_delta_before_discarding_leftover` и `gesture_phase_events_with_modifier_keep_their_delta` |
| R1-test 2: сырой `scroll_sensitivity` | закрыта | параметра `config` больше нет, `handle_scroll_wheel` читает `self.scroll_cfg`. Мутант M18 (`set_config` без `validated()`) убивают 5 тестов, M17 (без clamp) убивают 4 |
| R1-test 3: `scroll_sensitivity = nan` | закрыта | M15 (ветка NaN удалена) и M16 (NaN → 0) убивают по 3 теста: `config::tests::nan_scroll_sensitivity_falls_back_to_default` и два новых сквозных (LayoutManager и Reactor) |
| R1-test 5: `Began`/`Ended` без модификатора терялись | закрыта | мутанты M19–M22 (фазы отбрасываются / проходит только `Ended` / только `Began` / сохраняется дельта) убивает `gesture_phases_without_modifier_reset_progress`; M23 (у фаз с модификатором теряется дельта) убивает новый `gesture_phase_events_with_modifier_keep_their_delta` |
| R1-qa 1: порог трекпада ≈ 960 pt | закрыта (в коде) | порог 250 × 20 / s: M04/M05/M06 (250 → 251/249/300) убивают 13–21 тест, M07 (вернуть `W / min(колонок, 3)`) убивает 21, M11 (эталон 20 → 25) убивает 25. Сам порог 250 pt нужно откалибровать вживую (сценарий Ж1 R1-qa) |
| R1-qa 3: прогресс переживает жест без модификатора | закрыта | тот же набор, что для R1-test 5 |
| R1-test 4, 6, 7; R1-qa 2, 4, 5 | открыты, вне F1 | не проверялись, поведение не изменилось |

## Тесты с изменёнными ожиданиями

По каждому тесту: какое свойство он проверяет и какие мутанты его роняют (номера из раздела «Мутации»).

| Тест | Свойство | Падает на | Вывод |
|------|----------|-----------|-------|
| T1 `trackpad_scroll_moves_one_column_per_swipe_threshold_and_carries_remainder` | колонка на 250 pt, остаток переносится | M04, M06, M07, M11, **M27** (без переноса остатка) | не ослаблен |
| T1 `trackpad_scroll_threshold_scales_with_sensitivity` | при 40 порог 125 pt | M04, M06, M07, **M09** (без учёта чувствительности), M11 | не ослаблен |
| T1 `trackpad_momentum_neither_scrolls_nor_accumulates` | инерция не листает и не копится | **M26** (инерция учитывается), M04–M07 | не ослаблен |
| T1 `trackpad_gesture_start_resets_progress` | `Began` сбрасывает прогресс | M04, M06, M07, M11; **не падает на M03** (без сброса на `Began`) | ослаблен по смыслу, см. находку 1; свойство теперь проверяют `gesture_start_without_prior_end_resets_progress` (новый), `began_with_movement_…`, `gesture_phases_without_modifier_reset_progress` |
| R1 `zero_sensitivity_trackpad_never_scrolls_and_keeps_progress_finite` | при 0 не листает, прогресс конечен | **M09**, **M10** (делить порог вместо масштаба дельты), пороговые | не ослаблен; единственный тест, который ловит M10 |
| R1 `mouse_wheel_notch_after_partial_trackpad_swipe_moves_one_column` | деление колеса после неполного свайпа | **M01**, M04, M06, M07, M11 | не ослаблен |
| R1 `huge_trackpad_delta_moves_at_most_16_columns` | не больше 16 шагов | **M14** (16 → 17), пороговые | не ослаблен |
| R1 `zero_delta_phase_events_do_not_scroll` | нулевая дельта не листает и не сбрасывает | **M12** (сброс на нулевой дельте), пороговые | не ослаблен |
| R1 `began_with_movement_resets_progress_before_counting_its_delta` | сброс на `Began` до учёта его дельты | **M03**, пороговые | не ослаблен |
| R1 `reversing_direction_within_a_gesture_cancels_progress` | движение назад вычитается | только пороговые; **не падает на M25** (сброс при смене направления) | не ослаблен F1: со старыми числами (∓250/−299) было то же самое. Пробел закрыт новым `partial_reversal_subtracts_from_progress` |
| R1 `trackpad_invert_direction_applies_to_partial_progress` | инверсия для частичного прогресса | **M13** (у трекпада нет инверсии), пороговые | не ослаблен |
| R1 `switching_space_mid_gesture_does_not_carry_progress` | прогресс не переходит на другой стол | **M28** (один viewport на все раскладки), M04, M06, M07, M11 | не ослаблен |
| R1 `layout_kind_change_mid_gesture_is_safe` | смена вида раскладки без паники, после неё листается | M04–M07, M11 | не ослаблен. M28 он не ловит: перед проверкой идёт `Began`, а свойство теста — «безопасно», а не «прогресс не переносится» |
| R1 `two_columns_use_the_same_trackpad_threshold` | при 2 колонках порог тоже 250 pt | **M07** (при 2 колонках порог снова `W/2` = 450), пороговые | прежнее свойство «половина экрана» отменено решением F1 п. 5; новое свойство проверяется |

Ещё в двух тестах Reactor F1 поменяла только комментарии. `trackpad_gesture_scrolls_one_column_and_ignores_momentum` роняет M26. `huge_sensitivity_from_config_is_clamped_for_trackpad` роняют M17 и M18, но он проверяет только «−1 pt не листает». То, что при clamp до 100 колонку листают 50 pt, проверяет новый `huge_sensitivity_from_config_scrolls_one_column_per_50pt`.

## Добавленные тесты (9)

`src/actor/layout.rs`, `actor::layout::tests::r1_scroll_wheel::r2`:
- `gesture_end_counts_its_delta_before_discarding_leftover`: дельта `Ended` дотягивает до колонки, остаток после `Ended` отбрасывается
- `trackpad_swipe_per_column_is_250pt_times_20_over_sensitivity`: s = 10/40/100 → 500/125/50 pt, граница ±1 pt
- `nan_sensitivity_in_config_uses_default_trackpad_threshold`: NaN в конфиге → порог 250 pt через `set_config`
- `fast_wheel_uses_screen_column_step_not_trackpad_threshold`: не дискретное колесо считается против шага колонки 300 pt, а не 250
- `gesture_start_without_prior_end_resets_progress`: `Began` без предшествующего `Ended` сбрасывает прогресс
- `partial_reversal_subtracts_from_progress`: частичное движение назад вычитается из прогресса, а не обнуляет его

`src/actor/reactor.rs`, `actor::reactor::tests::scroll_wheel::r1::r2`:
- `gesture_phase_events_with_modifier_keep_their_delta`: `Began`/`Ended` с модификатором несут свою дельту
- `huge_sensitivity_from_config_scrolls_one_column_per_50pt`: 1e6 → clamp 100 → колонка на 50 pt
- `nan_sensitivity_from_config_uses_default_for_trackpad`: NaN через `ConfigChanged` → порог 250 pt, колесо листает

## Результат прогона

- Базовый прогон (`929065a`): 522 lib + 7 интеграционных, все прошли
- Итоговый прогон: **531 lib + 7 интеграционных, все прошли**
- `cargo build --all-targets`: без warnings; nightly `rustfmt --edition 2024 --check` по `layout.rs` и `reactor.rs`: чисто
- Стабильность: 3 прогона (2 полных, 1 `--lib`) и 30 прогонов `cargo test --lib` при мутациях дали одинаковый результат, нестабильных тестов нет

## Мутации

Ручные, скрипт `target/r2/mut.py` (в репозиторий не попадает), прогон `cargo test --lib --no-fail-fast` на каждый мутант, откат через `git checkout --`. После прогона `git status` по `src/` чист.

| # | Мутант | Убит | Кем (кратко) |
|---|--------|------|--------------|
| M01 | нет сброса на `Ended` | да (4) | `mouse_wheel_notch_after_partial…`, `trackpad_gesture_end_discards…`, r2 `gesture_end_counts…`, Reactor `gesture_phases_without_modifier…` |
| M02 | сброс на `Ended` до учёта дельты | да (2) | только новые r2: `gesture_end_counts…`, `gesture_phase_events_with_modifier_keep_their_delta` |
| M03 | нет сброса на `Began` | да (4) | `began_with_movement…`, `non_finite_deltas…`, r2 `gesture_start_without_prior_end…`, Reactor `gesture_phases_without_modifier…` |
| M04/M05/M06 | порог 251 / 249 / 300 | да (21/13/21) | тесты порога T1, R1 и r2 |
| M07 | трекпад снова на `W / min(колонок, 3)` | да (21) | включая `trackpad_threshold_does_not_depend_on_screen_width`, `two_columns_use_the_same_trackpad_threshold` |
| M08 | колесо на пороге 250 | да (2) | `zero_sensitivity_mouse_wheel…`, r2 `fast_wheel_uses_screen_column_step…` |
| M09 | трекпад без учёта чувствительности | да (5) | |
| M10 | делить порог на s вместо масштаба дельты | да (1) | `zero_sensitivity_trackpad_never_scrolls_and_keeps_progress_finite` |
| M11 | эталон 20 → 25 | да (25) | |
| M12 | нулевая дельта сбрасывает прогресс | да (1) | `zero_delta_phase_events_do_not_scroll` |
| M13 | трекпад без инверсии | да (2) | |
| M14 | ограничение 16 → 17 | да (2) | |
| M15 | нет замены NaN | да (3) | config-тест + 2 новых сквозных |
| M16 | NaN → 0 | да (3) | то же |
| M17 | нет clamp | да (4) | |
| M18 | `set_config` без `validated()` | да (5) | |
| M19 | `Began`/`Ended` без модификатора отбрасываются | да (1) | `gesture_phases_without_modifier_reset_progress` |
| M20 / M21 | без модификатора проходит только `Ended` / только `Began` | да (1 / 1) | то же |
| M22 | фазы без модификатора сохраняют дельту | да (1) | то же |
| M23 | фазы с модификатором теряют дельту | да (1) | только новый `gesture_phase_events_with_modifier_keep_their_delta` |
| M24 | `Changed` без модификатора тоже проходит с дельтой 0 | нет | **эквивалентный**: нулевая дельта без фазы ни на что не влияет |
| M25 | сброс прогресса при смене направления | да (1) | только новый `partial_reversal_subtracts_from_progress` |
| M26 | инерция учитывается | да (3) | |
| M27 | остаток после шага не переносится | да (3) | |
| M28 | один viewport на все раскладки | да (1) | `switching_space_mid_gesture_does_not_carry_progress` |

До новых тестов оставались живыми M02, M23 и M25. Mutation-инструмент (cargo-mutants) в проекте не настроен, не ставился.

## Покрытие

Покрытие изменённых строк не измерялось: `cargo-llvm-cov` и `tarpaulin` в окружении нет, ставить их я не стал (как и в круге 1). Вручную: каждая ветка, которую поменяла F1 (сброс на `Ended`, выбор порога, масштаб дельты, константа, замена NaN, фазы без модификатора в Reactor), покрыта мутантом, которого убивает хотя бы один тест.

## Находки

### 1. `trackpad_gesture_start_resets_progress` больше не проверяет сброс на `Began` — замечание

- **Где:** `src/actor/layout.rs:3734-3744` (тест T1)
- **Что:** перед `Began` в тесте идёт `Ended`, а после F1 прогресс сбрасывает уже `Ended`. Тест проходит и без сброса на `Began` (мутант M03).
- **Чем грозит:** имя теста обещает то, чего он не проверяет. Регрессии не пропускаются: M03 ловят 4 других теста, включая новый `gesture_start_without_prior_end_resets_progress`.
- **Предлагается:** оставить как сценарий реального жеста (`Ended` → `Began`) или переименовать. Тест я не менял.

### 2. Общий прогресс колеса и мыши с плавной прокруткой без фаз — замечание (продолжение R1-test 7 / R1-qa 5)

- **Где:** `src/actor/layout.rs:1499-1507`
- **Что:** continuous-события без фазы (Magic Mouse, Mos, Logi smooth) копят прогресс против порога 250 pt, и `Ended` его никогда не сбрасывает. Если такой мышью листнуть на 200 pt, а затем повернуть обычное колесо в обратную сторону, деление даст +300 − 200 = +100 < 300, и колонка не перелистнётся. Это та же проблема, что находка 1 круга 1, но для второго устройства без фаз. F1 отмечает это в «Рисках».
- **Чем грозит:** редкий случай, две мыши одновременно. Проглатывается одно деление.
- **Предлагается:** решить вместе с R1-qa 5 после живой проверки Ж6. Например, сбрасывать прогресс перед дискретным шагом колеса. Красного теста нет: без живого замера поведение таких мышей не установлено.

## Допущения

- Тесты с изменёнными ожиданиями я не менял. Пробелы закрыл новыми тестами во вложенных модулях `r2`, чтобы переиспользовать фикстуры круга 1.
- Статус PASS_WITH_FINDINGS, а не PASS, из-за находки 1 (содержание теста) и потому, что порог 250 pt ещё не откалиброван вживую (сценарий Ж1 R1-qa). Красных тестов нет, блокирующих находок нет.
- M24 признан эквивалентным: в Reactor `Changed` без модификатора с дельтой 0 проходит через `accumulate_scroll(0)` и не сбрасывает прогресс, поэтому наблюдаемое поведение не меняется.

## Было сломано до начала работы

Нет: базовый прогон 522 + 7 зелёный.
