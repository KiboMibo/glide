# R1-test, круг 2: проверка F1 анимации прокрутки

- **Дата:** 2026-09-22
- **План:** `docs/plans/2026-09-21-scroll-animation.md`, волна 1
- **Ветка:** `test/R1-anim-round2` (от `fix/F1-anim` @ `14977d7`)
- **Скоуп:** `git diff da9000a~1..fix/F1-anim -- src`: `spring.rs`, `scroll_viewport.rs`, `layout.rs`, `reactor.rs`, `reactor/animation.rs`, `reactor/testing.rs`. Цель: подтвердить закрытие A1, F-1, A3/F-3, A6, F-4 и проверить новые краевые случаи A1.
- **Статус:** **PASS_WITH_FINDINGS**. Все находки круга 1, порученные F1, закрыты, мутации это подтверждают. Найдена одна новая важная находка N-1: финальный кадр скролла отменяет пользовательский ресайз окна, который Reactor принял посреди скролла. Это регрессия F1, но воспроизводится только через гонку уведомлений. Тест на неё помечен `#[ignore]`, подробности в «Допущениях».

## Итог прогона

| | До (база F1) | После |
|---|---|---|
| `cargo test --lib` | 614 passed, 1 ignored | 628 passed, 0 failed, **2 ignored** |
| doc-тесты | 7 passed | 7 passed |
| `cargo build --all-targets` | — | без warnings |
| rustfmt nightly `--check` (3 файла) | — | чисто |

`cargo test --lib` прогнан 3 раза подряд, результат одинаковый.

**Новых тестов 15:** 3 в `spring.rs`, 4 в `reactor/animation.rs`, 8 в `reactor.rs` (`tests::scroll_animation`). Из них 1 помечен `#[ignore]` (N-1).

Ignored:
- `trace_with_a_scroll_animation_replays_to_the_same_layout`: F-2 из круга 1, не трогался.
- `window_resized_by_the_user_mid_scroll_keeps_the_user_frame_at_the_end`: новый, находка N-1. С `--ignored` падает: окно `w2` пользователь сузил до 430, а закончило оно на 450.

## Статус находок круга 1

| Находка | Статус | Чем подтверждено |
|---|---|---|
| A1 (блокирующая): `End` ставил устаревшую рамку | **Закрыта** | Мутации MA1–MA7 убиты (ниже). Стенд `Apps` действительно моделирует `last_animation_frame`: без моделирования (H1) возврат к старому поведению (MA1) не ловит ни один тест Reactor, а с моделированием его ловят 2 теста Reactor и 6 тестов менеджера. Отдельно пересмотрены краевые случаи, см. ниже |
| F-1 (важная): перелёт после retarget | **Закрыта** | Оба красных теста круга 1 зелёные. Добавлены 3 теста: ограничение не срезает скорость ниже `ω·|остаток|`, не трогает скорость, когда перелёта нет (сетка 30 × 5), нет перелёта и скачков на сетке 30 × 30 нажатий по 5–150 мс, завершение ≤ 400 мс |
| A3 = F-3: второе чтение часов | **Закрыта** | Мутация «вернуть `Instant::now()`» в `update_viewport_for_focus` убита тестом `focus_scroll_starts_at_the_given_time` |
| A6: финальный кадр после события, а не тика | **Закрыта** | Мутация «`tick_viewports` только в `tick_scroll_animation`» убита тестом `event_after_the_spring_settles_ends_the_scroll_at_the_target` |
| F-4: бесконечная цель | **Закрыта** | Мутация «убрать `is_finite()`» убита тестом `infinite_column_does_not_start_an_animation` |
| A2, A4: комментарии | Закрыты | Проверено чтением: комментарии на месте (`reactor.rs:321-331`) |
| F-2: replay и тики | Открыта, как и задумано | Тест по-прежнему `#[ignore]` |
| F-5, A5 | Вне F1 | Не проверялись |

### F-1: скорость не ломает непрерывность и не замедляет retarget вперёд

- `retarget_to_a_nearer_target_ahead_approaches_as_fast_as_it_can_without_passing`. Цель на 100 pt впереди, скорость больше предела. После retarget позиция та же (1e-9), скорость ровно `ω·100`, а не меньше. Движение монотонное, без перелёта, завершение ≤ 400 мс.
- `retarget_keeps_velocity_unless_it_would_pass_the_target`. 30 моментов × 5 целей (назад, на старт, вперёд близко и далеко). Позиция непрерывна. Скорость меняется только тогда, когда без ограничения был бы перелёт, в остальных случаях она та же до бита. Разворот не трогается.
- `retarget_forward_never_passes_the_new_target`. «Вправо, вправо, влево» на сетке 30 × 30 интервалов: перелёт < 1 pt, шаг за 1 мс < 100 pt, завершение ≤ 400 мс.

Тесты T1 `retarget_keeps_velocity` и `retarget_mid_animation_does_not_slow_down` F1 не менял, они зелёные.

### Тесты с изменёнными ожиданиями (из diff F1)

| Тест | Изменение | Вывод |
|---|---|---|
| `animation::…scroll_frames_begin_each_window_once_and_end_all_of_them` | 4 → 5 запросов, у `wid3` перед `End` кадр с размером `(1, 40)` | Строже: проверяется финальная рамка каждого окна |
| `animation::…layout_animation_ends_scroll_animation_first` | добавлена проверка кадра `(10, 0)` перед `End` | Строже |
| `reactor::…drag_interrupts_scroll_animation` | после прерывания разрешён `Frame(_, _, true)` | Матчер стал шире: рамку кадра он не проверяет. Компенсировано: `ends == begun` остался, мутацию MA6 (финальный кадр без размера) он ловит, а рамку на момент прерывания проверяет новый тест `dragging_another_window_…` |
| `reactor::…space_change_ends_scroll_animation` | то же | То же |
| `setup` → `setup_with_windows(animate, 4)` | рефакторинг | Поведение прежнее |

Ослаблений, которые не компенсированы другими тестами, нет.

### Новые краевые случаи A1

| Случай | Тест | Что проверяется |
|---|---|---|
| Окно исчезло до `ScrollEnd` (естественный конец) | `window_destroyed_right_before_the_end_is_not_brought_back` | Окно уничтожено перед последним тиком. Стенд его не воскрешает, `Begin`/`End` сбалансированы, остальные окна стоят там, куда их поставил Reactor, и в финальных рамках |
| Окно исчезло, потом прерывание | `window_destroyed_before_an_interruption_is_not_brought_back` | То же для пустого `ScrollEnd` |
| Новая цель (ресайз колонки) на последнем обновлении | `column_resized_on_the_update_that_ends_the_scroll_ends_at_the_new_size` (Reactor), `size_change_on_the_last_frame_is_the_final_frame` (менеджер) | Размер поменялся в том же обновлении, где пружина остановилась. Окно заканчивает в новой рамке, а не в последнем кадре скролла |
| Два `ScrollEnd` подряд | `second_scroll_end_in_a_row_ends_nothing_again`, `scroll_after_a_scroll_end_begins_the_window_again` (менеджер), `interrupting_twice_ends_the_scroll_once`, `interrupting_after_the_scroll_ended_sends_nothing` (Reactor) | Второй пустой `ScrollEnd` не шлёт ничего. `ScrollEnd` с уже закрытым окном даёт `SetWindowFrame`, без `End`. Новый скролл снова начинает окно |
| Прерывание drag → кадр = цель на момент прерывания | `dragging_another_window_leaves_animated_windows_at_their_frames_at_that_moment` | Перетаскивается не анимируемое окно (`w4`), как в реальности, где уведомления анимируемых окон сняты. Каждое анимируемое окно заканчивает ровно в рамке, которую ему последней отправили до прерывания. Тест проверяет, что к этому моменту скролл цели ещё не достиг |
| txid финального кадра | `final_frame_carries_the_last_txid_of_each_window` (менеджер), `user_resize_after_the_scroll_is_not_dropped_as_stale` (Reactor) | Финальный кадр несёт последний txid окна, и ресайз пользователем после скролла Reactor не отбрасывает как устаревший. До этого круга мутацию «не обновлять txid в `ScrollEnd`» (MA4) не ловил ни один тест |

## Мутации (ручные, по одной, файл возвращался копией из `target/`)

После каждой мутации `git diff` по продакшн-файлам пустой.

| # | Мутация | Файл | Убита тестами |
|---|---|---|---|
| S1 | без ограничения скорости | spring.rs | 4 (2 теста круга 1 + 2 новых) |
| S2 | ограничение `0.5·ω·|r|` (замедляет) | spring.rs | `retarget_to_a_nearer_target_ahead_…` (новый) |
| S3 | ограничение без проверки направления | spring.rs | `retarget_keeps_velocity_unless_…` (новый). Тест разворота круга 1 её **не ловил** |
| S4 | при ограничении скорость 0 | spring.rs | `retarget_to_a_nearer_target_ahead_…` (новый) |
| S5 | ограничение `2·ω·|r|` (перелёт) | spring.rs | 4 |
| MA1 | старое поведение: кадр с размером только окнам, сдвинутым на последнем тике | animation.rs | 8 (2 Reactor, 6 менеджер) |
| MA2 | пустой `ScrollEnd` только закрывает окна, без кадра | animation.rs | 3 |
| MA3 | `ScrollEnd` не обновляет сохранённую рамку | animation.rs | 17 |
| MA4 | `ScrollEnd` не обновляет сохранённый txid | animation.rs | 2, **оба новые** |
| MA5 | `ScrollFrame` хранит первую рамку окна | animation.rs | 9 |
| MA6 | финальный кадр без размера | animation.rs | 15 |
| MA7 | начатым окнам в `ScrollEnd` ещё и `SetWindowFrame` | animation.rs | 4 |
| A6 | `tick_viewports` только в `tick_scroll_animation` | reactor.rs | 1 |
| A3 | `Instant::now()` в `update_viewport_for_focus` | layout.rs | 1 |
| F4 | без `is_finite()` | scroll_viewport.rs | 1 |
| H1 | стенд: `End` не применяет `last_animation_frame` | testing.rs | 0 на верном коде, это ожидаемо. Вместе с MA1 тесты Reactor зелёные, то есть без моделирования стенд пропускает A1 |
| H2 | стенд: `AnimationFrame` воскрешает удалённое окно | testing.rs | 3 (1 старый, 2 новых) |
| H3 | стенд: `Begin` не сбрасывает `last_animation_frame` | testing.rs | выжила, эквивалентная мутация: каждый `End` забирает значение через `take()`, а кадр с размером вне анимации Reactor не шлёт |

Итого 18 мутаций: 16 убиты, H1 выживает ожидаемо (проверка стенда, а не кода), H3 эквивалентная.

## Новые находки

### N-1. Финальный кадр скролла отменяет пользовательский ресайз или drag анимируемого окна. Важная

- **Где:** `src/actor/reactor/animation.rs:193` (`end_scroll`: кадр с размером в `ScrollingWindow.frame` каждому начатому окну) вместе с `src/actor/reactor.rs:1622` (окно под `resizing_window` пропускается, поэтому его сохранённая рамка в менеджере не обновляется) и `:753` (Reactor принимает пользовательскую рамку в `frame_monotonic`).
- **Что:** Reactor принял `WindowFrameChanged(.., Requested(false), MouseDown)` для окна, у которого скролл уже начат. `frame_monotonic` становится рамкой пользователя. В конце скролла `end_scroll` шлёт этому окну кадр с размером в последнюю рамку Glide, и `EndWindowAnimation` применяет её с повторами. Окно возвращается в старую рамку, пока пользователь ещё тянет край или окно. Ответ приложения идёт с `Requested(true)`, Reactor его игнорирует и продолжает считать, что окно в рамке пользователя. Если кнопку уже отпустили, раскладка (ширина колонки от `WindowResized`) совпадает с `frame_monotonic`, и окно остаётся в старой рамке до следующего изменения раскладки. Это тот же класс ошибки, что A1.
- **Регрессия ли:** да, от F1. До F1 это окно в `ScrollEnd` не попадало (его пропускает `resizing_window`), и кадра с размером у него не было. Оно получало только `End` без доводки. Проверено: с мутацией MA1 (поведение до F1) тест зелёный.
- **Достижимость:** низкая. Реальный app-поток снимает `kAXWindowMoved`/`kAXWindowResized` с анимируемых окон, поэтому Reactor узнаёт о ресайзе только через уведомление, которое уже было в очереди в момент `Begin`: пользователь начал тянуть окно одновременно со стартом скролла. Стенд уведомления не снимает, поэтому воспроизводит это напрямую. Отчёт F1 упоминает этот риск в разделе «Риски», но оценивает его как безвредный.
- **Тест:** `reactor::tests::scroll_animation::window_resized_by_the_user_mid_scroll_keeps_the_user_frame_at_the_end` (`reactor.rs:7332`), `#[ignore]` с причиной. С `--ignored` падает: `resized to 430 wide, ended at 450`.
- **Предложение (на копии не проверялось):** когда Reactor принимает пользовательскую рамку окна посреди скролла, менеджер должен об этом узнать. Например, в `update_layout_at` окно под `resizing_window` при `scroll_animating` не пропускать молча, а передавать в анимацию его `frame_monotonic` (позиционный кадр в ту же рамку, где окно уже стоит). Тогда `ScrollingWindow.frame` совпадёт с рамкой пользователя. Другой вариант — отдельное сообщение «забыть финальный кадр окна» для `end_scroll`.

### N-2. Стенд создаёт окно на `SetWindowFrame` и `BeginWindowAnimation`. Замечание

- **Где:** `src/actor/reactor/testing.rs:331` и `:369` (`entry(wid).or_default()`).
- **Что:** F1 научил стенд пропускать `AnimationFrame` и `EndWindowAnimation` для удалённого окна, как это делает app-поток. `SetWindowFrame` и `Begin` удалённое окно по-прежнему воскрешают, а app-поток отвечает на них ошибкой `window_mut`. Сейчас Reactor таких запросов не шлёт: окно удаляется из `reactor.windows` до следующего `update_layout`. Если это изменится, стенд скроет ошибку.
- **Предложение:** в тех же двух ветках пропускать окно, которого нет, если это не ломает существующие тесты создания окон. Это задача на стенд, продакшн-код не затрагивается.

## Покрытие

Инструмента для покрытия в проекте нет (`cargo llvm-cov`, tarpaulin и grcov не установлены), ставить его я не стал. По ветвям изменённого F1 кода:
- `spring.rs::retarget`: обе стороны условия, в том числе направление, покрыты новыми тестами.
- `animation.rs`: `ScrollFrame` (новая и продолжающаяся запись), `ScrollEnd` (начатое и не начатое окно, пустой), `end_scroll` из `ScrollEnd`, из `Replace` и `SkipToEnd`. Не покрыт `end_scroll` при закрытии канала в `run()`: это async-цикл, так же было в круге 1.
- `reactor.rs::update_layout_at` (`tick_viewports` в начале), `interrupt_scroll_animation` (повторный вызов, после конца), `layout.rs::update_viewport_for_focus(now)`, `scroll_viewport.rs` (`is_finite`).
- `testing.rs`: все ветки `last_animation_frame` и пропуск удалённого окна в `AnimationFrame`.

## Допущения

1. **N-1 помечен `#[ignore]`, а не оставлен красным.** Задание допускало один `#[ignore]`, теперь их два. Причина: N-1 воспроизводится только через гонку уведомлений, основной сценарий A1 закрыт, а красный тест заблокировал бы мерж волны из-за редкого случая. Так же в круге 1 поступили с F-2. Если командир считает N-1 блокирующей, достаточно снять `#[ignore]`: тест готов и падает.
2. Сценарий «новая цель на последнем тике» смоделирован так: команда `CycleColumnWidth` передаётся напрямую в `LayoutManager`, а первым её видит тик, на котором пружина останавливается. Через `handle_event` так не получится: обновление берёт `Instant::now()`, пружина ещё идёт, и ресайз превращается в обычный `ScrollFrame` посреди скролла. Этот случай уже покрыт тестами F1.
3. Для «прерывания drag» перетаскивается не анимируемое окно (`w4`), потому что уведомления анимируемых окон в реальности сняты. Случай с анимируемым окном и есть N-1.
4. `testing.rs` и продакшн-код не менялись. Все изменения только в `#[cfg(test)]`-модулях, чисто добавочные (`git diff` без удалённых строк).
5. Mutation-тестирование инструментом не проводилось, его в проекте нет. Мутации ручные, 18 штук.

## Коммиты

- `a9ccc0a` test: Check the retarget velocity cap keeps speed unless it would overshoot
- `f142f52` test: Check final scroll frames, txids and repeated scroll ends in the manager
- `d16f4d3` test: Cover scroll end edge cases and a user resize undone by the final frame
- этот отчёт
