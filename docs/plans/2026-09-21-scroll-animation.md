# План: анимация прокрутки скролл-раскладки

- **Дата:** 2026-09-21
- **Статус:** выполнен, ожидает живой проверки
- **Цель:** переключение колонки в скролл-раскладке — быстрое, плавное, окна едут вместе; `animate = false` отключает и эту анимацию.

## 1. Контекст

Glide — тайлинговый WM для macOS на Rust. Прочитай `CLAUDE.md`, `ARCHITECTURE.md` (раздел «Scroll layout»), `agents.local.md`. Запрещено запускать `cargo run`, `glide launch`, WM. Форматирование — только `~/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/rustfmt --edition 2024 <изменённые .rs>`.

Жалоба пользователя (живая проверка, macOS 27): анимация медленная/запаздывает, дёргается (низкий FPS), окна рассинхронизированы.

Диагноз (разведка):
- **Пружина** `src/model/spring.rs:45-47` `with_defaults`: response 0.5 s, ζ=1.0, v0=0 → 36% пути за 100 мс; `is_complete` `:97-105` требует |x−target|<0.5 px и |v|<0.5 px/s → ~1 с хвоста. `retarget` `:49-56` перезапускает часы.
- **Viewport** `src/model/scroll_viewport.rs`: `animate_to` `:178-188`, `tick` `:215-222`, `apply_viewport_to_frames` `:241-280` (окна вне экрана паркуются за краем полностью). Возможная ошибка: на мониторе с `screen.origin.x != 0` смещение не учитывает origin (сравни `:224-230` и `src/model/size.rs:533`).
- **Доставка кадров** `src/actor/reactor.rs`: тик 1/120 с `:528-552` (перевзвод после работы) → `layout.tick_viewports()` → `update_layout(&[], true)`; в `update_layout` `:1614-1627` при активной скролл-анимации `AnimationMessage::SkipToEnd` → `Request::SetWindowFrame` каждому окну каждый тик. `SetWindowFrame` (`src/actor/app.rs:643-656`): переключение enhanced UI туда-обратно, set_size+set_position, чтение, до 3 повторов со сном 5 мс, без склейки, без подавления уведомлений. Лёгкий путь уже есть: `BeginWindowAnimation` / `AnimationFrame { set_size }` (склейка последнего кадра на окно в `app.rs:628-641`, `flush_all_frames` `:357-393`) / `EndWindowAnimation` (финальная рамка с повторами) — `app.rs:657-725`, `src/actor/reactor/animation.rs`.
- `GroupsUpdated` шлётся каждый тик (`reactor.rs:1549-1550`); `Instant::now()` берётся дважды на расчёт (`layout.rs:1338-1361`).
- `settings.animate` не влияет на пружину (только выбирает Replace/SkipToEnd для общей анимации).

## 2. Критерии готовности

1. Пружина: ≥ 80% пути за 150 мс, завершение ≤ ~350 мс для сдвига 1000 px, без перелёта; повторный ввод во время анимации не замедляет её заметно.
2. Во время скролл-анимации окнам уходят `BeginWindowAnimation` (один раз на окно), `AnimationFrame { set_size: false }` на тик и `EndWindowAnimation` по завершении; ни одного `SetWindowFrame` на промежуточных тиках.
3. `animate = false` → колонка переключается сразу (viewport прыгает в цель, без тиков).
4. `GroupsUpdated` не шлётся, если группы не изменились; один `now` на расчёт.
5. Record/replay не ломается; `cargo test` зелёный; тесты на пп. 1–4.

## 3. Скоуп

**Входит:** пп. 1–4, проверка и (если подтвердится) исправление ошибки origin на втором мониторе.
**Не входит:** синхронизация кадров через SkyLight (`SLSDisableUpdate`, транзакции, прокси-окна как в yabai), CVDisplayLink, настраиваемые параметры пружины, парковка окон с 1 px на экране.

## 4. Контракты

- `SpringAnimation::with_defaults` сохраняет сигнатуру; меняются константы и критерий `is_complete` (исполнитель T1 выбирает числа в рамках п.1 и описывает в отчёте).
- `ViewportState` публичный API не меняется, кроме, при необходимости, метода «прыгнуть в цель» (`snap_to_offset` уже есть — использовать его).
- T2 не меняет `spring.rs`/`scroll_viewport.rs`; T1 не меняет `reactor.rs`/`animation.rs`/`app.rs`/`layout.rs`.

## 5. Задачи

| ID | Название | Волна | Владеет |
|----|----------|-------|---------|
| T1 | Пружина и viewport | 1 | `src/model/spring.rs`, `src/model/scroll_viewport.rs`, `src/model/size.rs` (только если нужно для origin), `docs/reports/T1-anim-2026-09-21.md` |
| T2 | Лёгкая доставка кадров, animate=false, лишняя работа | 1 | `src/actor/reactor.rs`, `src/actor/reactor/animation.rs`, `src/actor/reactor/testing.rs`, `src/actor/app.rs` (если нужно), `src/actor/layout.rs`, `docs/reports/T2-anim-2026-09-21.md` |
| R1-test, R1-arch | Проверка | 1-ревью | тестовые модули; `docs/plans/reviews/scroll-animation-R1-*.md` |
| F1 | Исправления | 1-ревью | файлы T1/T2 |

### [x] T1. Пружина и viewport
Критерий 1 (тесты на время: доля пути на 50/100/150 мс, момент `is_complete`, отсутствие перелёта, retarget не сбрасывает скорость). Проверить ошибку origin на втором мониторе тестом; если подтвердится — исправить.

### [x] T2. Доставка кадров
Критерии 2–4. Подсказка: отдельное сообщение для `AnimationManager` (или прямой путь), которое не делает своё сглаживание, а шлёт готовые кадры скролла как `AnimationFrame` и корректно закрывает анимацию `EndWindowAnimation`; новые окна/исчезнувшие окна во время анимации; мышиный drag и `resizing_window` не ломаются; тесты стенда Reactor, проверяющие типы запросов на тиках.

### [x] T3. Подключить исправление origin (после T1, T2)
- **Владеет:** `src/actor/layout.rs`, `ARCHITECTURE.md`, `docs/reports/T3-anim-2026-09-21.md`.
- В `layout.rs` заменить `vp.set_screen_width(screen.size.width)` на `vp.set_screen(screen)` (`update_viewport_for_focus`, `handle_scroll_wheel`; найди все вызовы), удалить `set_screen_width`, если больше не используется (это `scroll_viewport.rs` — разрешено только удаление метода). Тест на уровне LayoutManager: скролл-раскладка на экране с origin.x=1920 и −1920 — колонки в пределах этого экрана. Коммит `fix: Keep scroll layout columns on their own screen with multiple monitors`.
- В `ARCHITECTURE.md`, раздел «Scroll layout» → «Viewport and animation»: 2–3 предложения о том, что кадры скролла идут через `AnimationManager` сообщениями `ScrollFrame`/`ScrollEnd` (position-only `AnimationFrame`, `EndWindowAnimation` в конце) и что offset отсчитывается от левого края экрана.

### [x] R1-test, R1-arch. Проверка
### [x] F1. Исправления
### [x] F2. Исправления (N-1 из второго круга)

## 6. Общая верификация
`cargo build --all-targets && cargo build --release && cargo test`, rustfmt `--check`. Живая проверка человеком: переключение колонки свайпом и клавиатурой, Chrome/Electron + нативные окна, `animate = false`.

## 7. Риски и точки остановки
- `EndWindowAnimation` включает enhanced UI обратно и делает финальную рамку с повторами — время финала может быть заметно; не должно быть «прыжка» в конце.
- Точки остановки: изменение контрактов п.4; если для п.2 нужна переделка `AnimationManager` шире одного нового сообщения — BLOCKED с вариантами.
