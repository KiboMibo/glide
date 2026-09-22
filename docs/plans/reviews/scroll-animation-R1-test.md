# R1-test: анимация прокрутки, волна 1

- **Дата:** 2026-09-21
- **План:** `docs/plans/2026-09-21-scroll-animation.md`, критерии 1–5
- **Ветка:** `test/R1-anim` (от `wave/scroll-animation-w1` @ `3bd571a`, merge T3 на месте)
- **Скоуп:** `git diff 9d40ef1..wave/scroll-animation-w1 -- src`: `spring.rs`, `scroll_viewport.rs`, `reactor.rs`, `reactor/animation.rs`, `reactor/testing.rs`, `layout.rs`
- **Статус:** **FAIL**. Найден один дефект: после retarget пружина перелетает цель (критерий 1, «без перелёта»). Остальные критерии 2–5 подтверждены тестами. Record/replay не падает, но при тиках скролла расходится с живой сессией. Это было и до волны, тест помечен `#[ignore]`.

## Итог прогона

| | До (база) | После |
|---|---|---|
| `cargo test --lib` | 585 passed | 605 passed, **2 failed**, 1 ignored |
| doc-тесты | 7 passed | 7 passed |
| `cargo build --all-targets` | без warnings | без warnings |
| rustfmt nightly `--check` (4 файла) | — | чисто |

Новые тесты стабильны: `cargo test --lib` прогнан 3 раза подряд, результат одинаковый.

**Новых тестов 23:** 7 в `spring.rs`, 6 в `scroll_viewport.rs`, 1 в `layout.rs`, 9 в `reactor.rs` (`tests::scroll_animation`).

Падают, это находка F-1:
- `model::spring::tests::quick_back_and_forth_retargets_do_not_overshoot`
- `model::scroll_viewport::tests::quick_right_right_left_does_not_scroll_past_the_column`

Помечен `#[ignore]` с причиной, это находка F-2:
- `actor::reactor::tests::scroll_animation::trace_with_a_scroll_animation_replays_to_the_same_layout`

## Находки

### F-1. Перелёт после retarget к цели, которая ещё впереди. Важная

- **Где:** `src/model/spring.rs:68-75` (`retarget`, строка 73 `self.initial_velocity = vel;`).
- **Что:** `retarget` сохраняет текущую скорость целиком. Если новая цель лежит по ходу движения и ближе, чем `|v|/ω`, критически демпфированная пружина её проскакивает. Сценарий: вправо, вправо и сразу влево (0 → 960 → 1920 → 960). В момент нажатия «влево» viewport ещё не дошёл до 960, но летит на него со скоростью в тысячи pt/s.
- **Замеры** (пружина по умолчанию, шаг колонки 960 pt):

  | 2-е нажатие после 1-го | 3-е после 2-го | перелёт |
  |---|---|---|
  | 30 мс | 30 мс | 106 pt |
  | 50 мс | 15 мс | 24 pt |
  | 50 мс | 30 мс | **173 pt** |
  | 80 мс | 15 мс | 79 pt |

  На уровне `ViewportState` (`CenterMode::Always`, колонки по 960 pt на экране 1920) viewport уходит на 173 pt дальше колонки и потом возвращается. Такой отскок хорошо видно. С `center_focused_column = "never"` (значение по умолчанию) это случается, когда колонка шире половины экрана (пресеты 0.667 и 1.0): тогда каждое нажатие сдвигает цель.
- **Чем грозит:** расходится с критерием 1 («без перелёта»). Отчёт T1 утверждает, что перелёт невозможен. Для «ещё колонку в ту же сторону» это верно, а для разворота на цель, лежащую по ходу, нет.
- **Предложение (проверено на копии репозитория):** в `retarget` ограничить составляющую скорости в сторону новой цели значением `ω·|остаток|`. Это граница, при которой критически демпфированная пружина не пересекает цель:
  ```rust
  let mut vel = self.velocity_at(now);
  let remaining = new_target - current;
  let max_toward = self.omega_n * remaining.abs();
  if vel * remaining > 0.0 && vel.abs() > max_toward {
      vel = max_toward * remaining.signum();
  }
  ```
  На копии (`git worktree` в `target/`, удалена) с этой правкой прошёл весь набор: 607 passed, 0 failed. Тесты править не пришлось, тесты T1 (`retarget_keeps_velocity`, `retarget_mid_animation_does_not_slow_down`) тоже зелёные. Скорость по-прежнему непрерывна по модулю в обычном случае. Ограничение срабатывает только тогда, когда без него был бы перелёт.

### F-2. Replay расходится с живой сессией после тиков скролла. Важная, была до волны

- **Где:** `src/actor/reactor.rs:586` (`tick_scroll_animation`: тики не попадают в `Record`) и `:430` (`next_txid` на каждый кадр тика). Сравнение идёт в `:737`.
- **Что:** на каждом тике каждое сдвинутое окно получает `next_txid()`. В replay тиков нет, поэтому `last_sent_txid` там меньше. Следующее пользовательское изменение рамки (`WindowFrameChanged` с актуальным txid) replay отбрасывает как устаревшее. Тест `trace_with_a_scroll_animation_replays_to_the_same_layout`: после скролла пользователь сужает колонку до 350 pt. В живом reactor ширина 350, в replay осталась 450.
- **Регрессия ли:** нет. До волны тики так же не записывались и так же увеличивали txid (раньше через `SetWindowFrame`). Replay не падает (это заодно проверяет `Drop` каждого тестового reactor), но проигрывает не то. Критерий 5 («не ломается») в узком смысле выполнен.
- **Предложение:** записывать тик в `Record` как событие, например `Event::ScrollTick(Instant-offset)`, и проигрывать через `tick_scroll_animation`. Второй вариант: не расходовать txid на промежуточные кадры скролла, а брать новый txid только в `ScrollEnd`. Решать F1 или человеку; это шире волны 1.

### F-3. Второе чтение часов на расчёт. Замечание

- **Где:** `src/actor/layout.rs:1430`: `update_viewport_for_focus` передаёт `Instant::now()` в `ensure_column_visible`. Её вызывает `Reactor::update_layout_at`, у которого уже есть свой `now`.
- **Чем грозит:** критерий 4 («один `now` на расчёт») выполнен не полностью. Старт или retarget пружины берёт своё время, а кадры считаются по `now` из reactor. В тестах со синтетическими тиками из-за этого пружина стартует «в будущем» относительно `now` тика (`duration_since` насыщается, паники нет). Предложение: передавать `now` параметром.

### F-4. Нечисловая цель прокрутки даёт вечную анимацию. Замечание

- **Где:** `src/model/scroll_viewport.rs:165`. NaN отсекается сравнением `> 0.5`, а `±inf` проходит. `SpringAnimation` с бесконечной целью никогда не `is_complete`, и тогда `has_active_scroll_animation` остаётся true, а тики идут на 120 Гц без конца.
- **Достижимость:** из нормальной раскладки `inf` в `column_x` не попадает, поэтому красного теста нет. Проверено: NaN не паникует и не запускает анимацию (`nan_target_does_not_panic`, `non_finite_column_does_not_start_an_animation`). Предложение: `if !new_offset.is_finite() { return; }`, по принципу защиты в глубину из CLAUDE.md.

### F-5. Парковка на соседний монитор. Замечание, было до волны, вне скоупа

- **Где:** `src/model/scroll_viewport.rs:280` (и симметрично для левого края). Окно, запаркованное вплотную к краю экрана, при соседнем мониторе целиком оказывается на нём. В плане это явно отнесено к «не входит» (парковка с 1 px). Записано, чтобы не потерялось.

## Что покрыто (по заданию)

| Требование | Тесты |
|---|---|
| Пружина: доли пути, время завершения, без перелёта | T1: `default_spring_*`. Новые: `completion_time_stays_short_for_small_and_large_moves` (1…3000 pt, ≤400 мс), `zero_distance_spring_completes_without_moving` |
| Retarget, в том числе в обратную сторону | `retarget_in_the_opposite_direction_turns_around_smoothly`: скачок за 1 мс < 30 pt, по инерции не дальше 5% пути, нет перелёта через новую цель, завершение ≤400 мс. **`quick_back_and_forth_retargets_do_not_overshoot`: красный (F-1)**. `retarget_while_animating_keeps_the_offset_continuous` |
| Вырожденные случаи | `nan_target_does_not_panic`, `time_before_the_start_does_not_panic`, `non_finite_column_does_not_start_an_animation`, `column_already_visible_does_not_start_an_animation`, `is_not_complete_while_passing_the_target_fast` |
| Begin/End: drag, смена стола и экрана, окно исчезло, animate выключили | тесты T2 (`drag_…`, `space_change_…`, `screen_change_…`, `destroyed_window_…`, `disabling_animate_…`) |
| Begin/End: новое окно посреди анимации | `window_appearing_mid_scroll_joins_the_animation`: `Begin` и `End` чередуются. Единственная запись с размером на промежуточных тиках — один кадр нового окна (его размер меняется) |
| Begin/End: вторая анимация до конца первой (retarget) и разворот | `second_move_mid_scroll_retargets_without_restarting_windows` (у каждого окна ровно один `Begin`), `reversing_mid_scroll_ends_every_window_once` |
| Begin/End: `resizing_window` посреди скролла | `window_resized_by_the_user_mid_scroll_still_gets_its_end`: окну под ресайзом кадры не идут, `End` приходит |
| Replace во время скролла и наоборот | T2: `layout_animation_ends_scroll_animation_first`, `scroll_frame_finishes_layout_animation_first` (уровень `AnimationManager`). В reactor при активном скролле любой `update_layout` становится `ScrollFrame`, поэтому `Replace` поверх скролла из reactor не возникает |
| Нет `SetWindowFrame` на промежуточных тиках | общий помощник `assert_no_full_frame_before_last` во всех новых тестах reactor |
| Парковка | T2 `window_gets_no_frames_once_it_is_parked`. Новый: запаркованные окна стоят ровно у края своего экрана |
| `animate = false`, включение на лету | `animate_false_never_starts_an_animation` (5 переключений, ни одного `Begin`/`Frame`), `enabling_animate_on_the_fly_animates_the_next_scroll` |
| `GroupsUpdated` и сброс кеша | `scroll_ticks_do_not_resend_unchanged_groups`, `group_cache_is_cleared_by_screen_and_config_changes` (ровно один повтор после `ScreenParametersChanged` и после `ConfigChanged`, потом тишина). T2: `SpaceChanged` |
| Origin ≠ 0, два экрана со скроллом одновременно | `two_scroll_layouts_animating_at_once_stay_on_their_own_screens`: экраны с origin −1920 и 1920, обе анимации активны. Каждые 10 мс до 500 мс каждое окно либо пересекает свой экран, либо запарковано ровно у его края. В конце окно в фокусе внутри экрана. `moving_the_screen_keeps_columns_on_it`, `frames_mid_animation_are_between_start_and_end` |
| Record/replay | Каждый тестовый reactor в `Drop` проигрывает свою запись, в том числе все сценарии скролла: без паник. `trace_with_a_scroll_animation_replays_to_the_same_layout`: ignored (F-2) |

## Мутации (ручные, по одной, файл возвращался через `git checkout`)

Прогнано 18 мутаций, убиты все 18. Базовые красные тесты F-1 из подсчёта исключены.

| # | Мутация | Файл | Убита тестами |
|---|---|---|---|
| M1 | порог скорости в `is_complete` отключён | spring.rs | `is_not_complete_while_passing_the_target_fast` |
| M2 | `retarget` обнуляет скорость | spring.rs | `retarget_keeps_velocity` |
| M3 | response 0.5 с | spring.rs | 5 тестов |
| M4 | `column_x` без origin | scroll_viewport.rs | 6 |
| M5 | `view_bounds` без origin | scroll_viewport.rs | 5 |
| M6 | правая парковка +1 pt | scroll_viewport.rs | 2 (в том числе новый тест на два экрана) |
| M7 | `Begin` на каждом кадре | animation.rs | 9 |
| M8 | промежуточные кадры с `set_size: true` | animation.rs | 8 |
| M9 | `ScrollEnd` не шлёт `End` | animation.rs | 12 |
| M10 | `Replace`/`SkipToEnd` не закрывают скролл | animation.rs | 1 |
| M11 | кеш групп не сбрасывается на `ScreenParametersChanged` | reactor.rs | `group_cache_is_cleared_…` (новый) |
| M12 | кеш групп не сбрасывается на `ConfigChanged` | reactor.rs | `group_cache_is_cleared_…` (новый) |
| M13 | дедупликация групп выключена | reactor.rs | 3 |
| M14 | при `animate=false` нет снапа при фокусе | layout.rs | 2 |
| M15 | конец скролла уходит как `SkipToEnd` | reactor.rs | 4 |
| M16 | окну под ресайзом пишутся кадры | reactor.rs | `window_resized_…` (новый) |
| M17 | кадры считаются по второму `Instant::now()` | layout.rs | 8 |
| M18 | новым окнам не шлётся `Begin` | animation.rs | 13 |

M11, M12 и M16 до этой работы выживали: ни один тест T1–T3 их не ловил.

## Покрытие

Инструмента для покрытия в проекте нет (`cargo llvm-cov`, tarpaulin и grcov не установлены), ставить его ради одного прогона я не стал. Число по изменённым строкам не посчитано. Вместо него покрытие перечислено по ветвям:
- `animation.rs`: все ветви `ScrollFrame`/`ScrollEnd`/`end_scroll` и перекрёстные ветви `Replace`/`SkipToEnd` покрыты. Не покрыт `run()`: `end_scroll` при закрытии канала, это async-цикл.
- `reactor.rs`: `update_layout_at` (все 5 ветвей выбора сообщения), `interrupt_scroll_animation`, `same_groups`, снапы на трёх событиях. Не покрыта ветка `send_animation` при закрытом канале: там только лог.
- `layout.rs`: `snap_viewports`, `has_active_scroll_animation(now)`, `tick_viewports(now)`, `set_screen` в обоих вызовах.
- `spring.rs` и `scroll_viewport.rs`: весь изменённый код.

## Допущения

1. **F-2 помечен `#[ignore]`, а не оставлен красным.** Дефект был до волны и не вызван её изменениями, а чинить его придётся шире F1: менять формат записи или политику txid. Красный тест заблокировал бы мерж волны из-за старой проблемы. Тест виден в выводе `cargo test` как `ignored` вместе с причиной.
2. **F-1 оставлен красным**, отсюда статус FAIL. Критерий 1 прямо требует «без перелёта», а сценарий «вправо-вправо-влево» реалистичен при широких колонках или `center_focused_column = always`. Серьёзность «важная», а не «блокирующая»: основной сценарий (одиночные нажатия, повтор в одну сторону) работает правильно.
3. Порог «перелёт < 1 pt» в тестах F-1 выбран потому, что при правильном ограничении скорости перелёт равен 0, а 1 pt оставлен на погрешность вычислений с плавающей точкой.
4. Проверка «нет полной записи на промежуточных тиках» разрешает один кадр `AnimationFrame { set_size: true }` новому окну, у которого меняется размер. `SetWindowFrame` там нет. Это соответствует дизайну T2 (`set_size` только при изменении размера).
5. Тесты добавлены только в `#[cfg(test)]`-модули. `testing.rs` не менялся: хватило существующего стенда `AnimationPump`.
6. Сценарий «Replace во время скролла» на уровне reactor недостижим: при активном скролле `update_layout` всегда шлёт `ScrollFrame`. Поэтому хватает тестов T2 на уровне `AnimationManager`, и новых тестов на это я не писал.

## Коммиты

- `fcec36e` test: Cover scroll spring retargets and viewport edge cases
- `b967caf` test: Check two scroll layouts animating on offset screens at once
- `b78d803` test: Check scroll animation begin/end balance on interruptions
- этот отчёт
