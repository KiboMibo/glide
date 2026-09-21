# План: scratchpad-окна в Glide

- **Дата:** 2026-09-16
- **Статус:** выполнен (ожидает живой проверки и мержа в main)
- **Цель:** по хоткею показывать и прятать отдельное приложение (Keyguard, Spotify, Reeder…) поверх текущего рабочего стола, как `scratchpad` в yabai.

## 1. Контекст

Glide — тайлинговый оконный менеджер для macOS на Rust. Перед работой прочитай `CLAUDE.md` и `ARCHITECTURE.md` в корне: там слои (`sys` → `model` → `actor`), правила комментариев, коммитов и логов. Запрещено запускать `cargo run`, `glide launch` и любым способом стартовать живой оконный менеджер. `cargo run --example devtool -- …` разрешён.

Сборка и проверки: `cargo build`, `cargo test`, форматирование — `~/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/rustfmt --edition 2024 <изменённые .rs файлы>` (НЕ `cargo fmt`: на этой машине он подхватывает stable rustfmt и переформатирует весь репозиторий).

Что уже есть и на что опираемся:

- **Плавающие окна** живут только в `LayoutManager` (`src/actor/layout.rs`): `floating_windows` (~229), `floating_restore_frames` (~233), `active_floating_windows` (~235), `last_floating_focus` (~241). Рамку плавающему окну задают одноразовые `EventResponse.frame_overrides` (~116); Reactor применяет их в `update_layout` (`src/actor/reactor.rs` ~1206). Прецедент — восстановление рамки в `ToggleWindowFloating` (layout.rs ~804-825).
- **Правила окон:** `WindowRule` (`src/config.rs` ~157), классификация `classify_window` (layout.rs ~266-353) → `WindowClass {Untracked, FloatByDefault, Regular}`; вызывается из `handle_event` на `WindowAdded`/`WindowsOnScreenUpdated`/`WindowSpaceChanged`.
- **Команды:** `WmCommand` (untagged: `WmCmd` | `reactor::Command`), `reactor::Command` (untagged: `LayoutCommand` | `MetricsCommand` | `ReactorCommand`), `ReactorCommand` в reactor.rs ~217. Reactor обрабатывает команды в reactor.rs ~836-869. Хоткеи — `livesplit-hotkey` (⌘ = `Meta`).
- **Фокус окна фонового приложения** уже работает: `EventResponse.focus_window` → RaiseManager → `app::Request::Raise` → `make_key_window` (`src/sys/window_server.rs` ~251).
- **Запуск программ:** `exec` в `src/actor/wm_controller.rs` ~284-313 и `src/sys/bundle.rs` (без прав Glide на Accessibility).
- **Чего нет:** запуска/скрытия/показа приложения по bundle id; реакции на Cmd+H (скрытие приложения не отслеживается — `src/actor/app.rs` ~240-252 не подписан на `kAXApplicationHiddenNotification`); переноса окна между рабочими столами macOS (ни одного вызова SkyLight для этого; объявленные приватные API — `src/sys/window_server.rs` ~278-329, `src/sys/screen.rs` ~349-358).
- **Тестовый стенд Reactor:** `src/actor/reactor/testing.rs` (`Apps`, `make_app`, `simulate_until_quiet`); `Request::Raise` там `todo!()`. Образцы тестов: reactor.rs `floating_window_restores_its_last_user_frame` (~1361), layout.rs `floating_windows` (~2117), правила — layout.rs ~1754-1910, config.rs `window_rules_parse` (~470-760).

Окружение пользователя: macOS 27.0, SIP в частично отключённом состоянии (Filesystem/Debugging/NVRAM protections disabled), установлен `yabai.osax`, yabai не запущен.

## 2. Цель и критерии готовности

1. В конфиге правило `[[window_rules]]` с `scratchpad = "<имя>"` помечает окна приложения как scratchpad; такие окна всегда плавающие и не участвуют в раскладке.
2. Хоткей `{ toggle_scratchpad = { name = "<имя>", launch = "<bundle id>" } }`:
   - окна нет → приложение запускается; когда его окно появится, оно показывается по правилам п. «показ»;
   - окно есть, но приложение скрыто, окно не в фокусе или на другом рабочем столе → «показ»: приложение раскрывается, окно получает рамку `frame` (доли активного экрана), фокус, и (волна 4) оказывается на текущем рабочем столе;
   - окно видно и в фокусе → приложение скрывается (как Cmd+H).
3. `cargo test` зелёный, новые модули покрыты тестами, `glide.default.toml` документирует новые поля.
4. Пользователь на своей машине: ⌘⌥K показывает и прячет Keyguard.

## 3. Скоуп

**Входит:** правило `scratchpad` + `frame`, команда `toggle_scratchpad`, скрытие/показ через приложение, отслеживание скрытия приложения, перенос окна на текущий рабочий стол (волна 4, если T3 найдёт способ без инъекции в Dock).

**Не входит:** несколько окон одного scratchpad (берётся одно — первое зарегистрированное); scratchpad для окон без правила (команда «сделать текущее окно scratchpad»); анимация появления; поддержка scripting addition/`yabai.osax`; сохранение состояния scratchpad через `save_and_exit`/restore; PR в upstream.

## 4. Архитектурные решения

- **Scratchpad-окно = плавающее окно с именем.** Отдельного «скрытого рабочего стола» нет. Почему: плавающие окна уже умеют жить вне дерева и получать рамку через `frame_overrides`.
- **Решение «запустить / показать / спрятать» — чистая функция в `src/model/scratchpad.rs`.** LayoutManager владеет реестром, Reactor собирает факты (скрыто ли приложение, в фокусе ли окно, видно ли на экране) и исполняет эффекты. Почему: правило проекта — политика в LayoutManager, модель без побочных эффектов.
- **Прятать — скрывать приложение целиком** (`NSRunningApplication.hide/unhide`), а не сворачивать окно и не уводить его за край. Выбор пользователя.
- **Скрытие/показ выполняет поток приложения** (новый `app::Request::SetHidden`), он же сообщает о смене состояния (`kAXApplicationHiddenNotification`/`kAXApplicationShownNotification`). Почему: поток приложения — единственный владелец связи с процессом; так Reactor узнаёт и о ручном Cmd+H.
- **Запуск** — `sys::app::launch_app(bundle_id)` через `/usr/bin/open -b` в отдельном потоке (как `exec`), чтобы приложение не наследовало права Glide.
- **Команда — вариант `ReactorCommand`**, т.к. нужна вся информация Reactor.
- **Отвергнуто:** увод окна за край экрана (macOS оставляет край видимым, приложения сопротивляются); сворачивание в Dock (анимация, иконка окна в Dock); `yabai.osax` (чужой протокол; только по решению пользователя).

## 5. Контракты

Фиксируются здесь; задачи обязаны им следовать. Изменение контракта — точка остановки.

### 5.1 Конфиг (T1)

```toml
[[window_rules]]
if.app_id = "com.example.keyguard"
scratchpad = "keyguard"
frame = { x = 0.1, y = 0.1, width = 0.8, height = 0.8 }   # необязательно
```

```rust
// src/config.rs
pub struct WindowRule {
    #[serde(rename = "if", default)]
    pub conditions: WindowRuleConditions,
    /// Было `bool` (обязательное). Теперь необязательное.
    #[serde(default)]
    pub float: Option<bool>,
    #[serde(default)]
    pub scratchpad: Option<String>,
    #[serde(default)]
    pub frame: Option<crate::model::scratchpad::FractionalRect>,
}
```

Валидация (ошибка конфига, как у невалидного `title_regex`): у правила есть хотя бы одно из `float`/`scratchpad`; `scratchpad` вместе с `float = false` — ошибка; `frame` без `scratchpad` — ошибка; пустое имя — ошибка. Существующие конфиги с `float = true/false` работают как раньше.

### 5.2 Модель (T1) — `src/model/scratchpad.rs`, `pub mod scratchpad` в `src/model.rs`

```rust
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FractionalRect { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }

impl FractionalRect {
    /// x=0.1, y=0.1, width=0.8, height=0.8 — используется, когда `frame` не задан.
    pub const DEFAULT: FractionalRect;
    /// Значения зажимаются в [0, 1], width/height не меньше 0.05, x+width и y+height не больше 1.
    pub fn validated(self) -> FractionalRect;
    /// Прямоугольник внутри `screen` (координаты как у CGRect в Glide).
    pub fn to_frame(&self, screen: CGRect) -> CGRect;
}

/// Факты о текущем окне scratchpad, которые собирает Reactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchpadWindowState {
    pub app_hidden: bool,
    pub focused: bool,
    pub on_active_space: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScratchpadAction {
    Launch,
    Show(WindowId),
    Hide(WindowId),
}

#[derive(Debug, Default)]
pub struct Scratchpads { /* name -> (WindowId, FractionalRect); pending show по имени */ }

impl Scratchpads {
    /// Регистрирует окно под именем. Если под именем уже живое окно — оставляет старое, возвращает false.
    pub fn register(&mut self, name: &str, wid: WindowId, frame: FractionalRect) -> bool;
    /// Забывает окно (окно уничтожено). Возвращает имя, если оно было scratchpad.
    pub fn remove_window(&mut self, wid: WindowId) -> Option<String>;
    /// Все окна приложения исчезли (приложение завершилось).
    pub fn remove_app(&mut self, pid: pid_t);
    pub fn name_of(&self, wid: WindowId) -> Option<&str>;
    pub fn window(&self, name: &str) -> Option<(WindowId, FractionalRect)>;
    pub fn is_scratchpad(&self, wid: WindowId) -> bool;
    /// Hide, если state = {app_hidden: false, focused: true, on_active_space: true}; иначе Show; нет окна — Launch
    /// и запомнить «показать при регистрации».
    pub fn toggle(&mut self, name: &str, state: impl Fn(WindowId) -> ScratchpadWindowState) -> ScratchpadAction;
    /// true, если для имени был отложенный показ (после Launch); флаг сбрасывается.
    pub fn take_pending_show(&mut self, name: &str) -> bool;
}
```

### 5.3 Команда (T1) — `src/actor/reactor.rs`

```rust
pub enum ReactorCommand {
    Debug, Serialize, SaveAndExit,
    ToggleScratchpad {
        name: String,
        /// Bundle id для запуска, если окна нет. Без него команда при отсутствии окна только пишет warn.
        #[serde(default)]
        launch: Option<String>,
    },
}
```
TOML: `"Meta + Alt + K" = { toggle_scratchpad = { name = "keyguard", launch = "com.example.keyguard" } }`. В T1 Reactor для этой команды только пишет `warn!("toggle_scratchpad is not implemented yet")`; реальная обработка — T5.

### 5.4 sys (T2) — `src/sys/app.rs`

```rust
/// NSRunningApplication.hide / unhide. false, если процесса нет или macOS отказала.
pub fn set_app_hidden(pid: pid_t, hidden: bool) -> bool;
/// NSRunningApplication.isHidden; None, если процесса нет.
pub fn is_app_hidden(pid: pid_t) -> Option<bool>;
/// NSRunningApplication.activateWithOptions (без ActivateAllWindows). false при ошибке.
pub fn activate_app(pid: pid_t) -> bool;
/// Запуск по bundle id через `/usr/bin/open -b <id>` в отдельном потоке; не блокирует.
/// bundle_id проверяется: только [A-Za-z0-9.-], непустой, иначе Err.
pub fn launch_app(bundle_id: &str) -> Result<(), LaunchError>;
```

### 5.5 Поток приложения (T4) — `src/actor/app.rs`, `src/actor/reactor.rs`

```rust
// app::Request
SetHidden(bool),
// reactor::Event
ApplicationHiddenChanged(pid_t, bool),
```
Поток приложения подписывается на `kAXApplicationHiddenNotification` и `kAXApplicationShownNotification` и шлёт `ApplicationHiddenChanged`. Начальное значение — `AppInfo`/`ApplicationLaunched` дополняется полем `is_hidden: bool` (или Reactor спрашивает `is_app_hidden` при запуске приложения — на выбор T4, записать в отчёт). Reactor хранит флаг в состоянии приложения (`AppState`) и отдаёт его наружу методом `fn is_app_hidden(&self, pid) -> bool`.

### 5.6 Перенос между рабочими столами (T3 → T6)

T3 создаёт `src/sys/space_move.rs` с функцией (сигнатура может уточниться по итогам T3, это не контракт для волн 1–3):
```rust
/// Переносит окно на указанный рабочий стол. Err — способ недоступен на этой системе.
pub fn move_window_to_space(wsid: WindowServerId, space: SpaceId) -> Result<(), SpaceMoveError>;
```

## 6. Git

Рабочая ветка `develop` (от `main`, локально). Ветки волн `wave/scratchpad-w<N>` от `develop`. Ветки задач `feat/T<N>-<слаг>` от ветки волны. Каждый исполнитель работает в своём git worktree. **Ничего не пушить.**

## 7. Карта задач

| ID | Название | Зависит от | Волна |
|----|----------|------------|-------|
| T1 | Конфиг, команда, модель `scratchpad` | — | 1 |
| T2 | sys: скрыть/показать/активировать/запустить приложение | — | 1 |
| T3 | Разведка: перенос окна на текущий рабочий стол на macOS 27 | — | 1 |
| R1-test, R1-sec | Проверка волны 1 | T1, T2, T3 | 1-ревью |
| F1 | Исправления волны 1 | R1-* | 1-ревью |
| T4 | Поток приложения: `SetHidden`, уведомления скрытия | T2 | 2 |
| R2-test | Проверка волны 2 | T4 | 2-ревью |
| F2 | Исправления волны 2 | R2-* | 2-ревью |
| T5 | Интеграция в LayoutManager и Reactor | T1, T2, T4 | 3 |
| R3-test, R3-arch, R3-sec, R3-qa | Проверка волны 3 | T5 | 3-ревью |
| F3 | Исправления волны 3 | R3-* | 3-ревью |
| T6 | Перенос окна на текущий рабочий стол | T3, T5, решение пользователя | 4 |
| R4-test, R4-qa | Итоговая проверка и документация | T6 | 4-ревью |
| F4 | Исправления волны 4 | R4-* | 4-ревью |

Владение файлами в волне 1 не пересекается: T1 — `config.rs`, `glide.default.toml`, `model.rs`, `model/scratchpad.rs`, `reactor.rs` (только `ReactorCommand`), `layout.rs` (только адаптация `classify_window` и литералов `WindowRule` в тестах); T2 — `sys/app.rs`; T3 — `sys.rs`, `sys/space_move.rs`, `examples/devtool.rs`, `docs/plans/research/`.

## 8. Задачи

### [x] T1. Конфиг, команда, модель scratchpad

- **Зависит от:** —
- **Волна:** 1
- **Файлы (владеет):** `src/config.rs`, `glide.default.toml`, `src/model.rs`, `src/model/scratchpad.rs` (новый), `src/actor/reactor.rs` (только enum `ReactorCommand` и ветка его обработки), `src/actor/layout.rs` (только `classify_window`/`window_rule_matches` и литералы `WindowRule` в тестах)
- **Файлы (только читает):** `src/actor/wm_controller.rs`, `src/sys/geometry.rs`, `src/config/partial.rs`
- **Скилы:** `/coding-writer`

**Что сделать.** Реализовать контракты 5.1, 5.2, 5.3. В `classify_window` правило со `scratchpad` классифицирует окно как плавающее (`FloatByDefault`); `float: None` без `scratchpad` не бывает после валидации. В `glide.default.toml` дописать в блок комментариев про `window_rules` описание `scratchpad` и `frame`, в блок experimental/keys — закомментированный пример `toggle_scratchpad`. Тесты модели — чистые unit-тесты в `scratchpad.rs`.

**Критерии приёмки**
- Старые правила с `float = true/false` парсятся и классифицируются как раньше (существующие тесты зелёные без изменения ожиданий).
- Правило со `scratchpad` и без `float` парсится; окно классифицируется как плавающее.
- Ошибки конфига: без `float` и без `scratchpad`; `scratchpad` + `float = false`; `frame` без `scratchpad`; `scratchpad = ""`. На каждую — тест.
- `{ toggle_scratchpad = { name = "k", launch = "com.x" } }` и без `launch` разбираются в `WmCommand::ReactorCommand(Command::Reactor(ReactorCommand::ToggleScratchpad{..}))` — тест.
- `FractionalRect::to_frame` на экране `(0,0,1000,800)` и `DEFAULT` даёт `(100,80,800,640)`; на экране со смещением учитывает origin; `validated` зажимает выход за границы — тесты.
- `Scratchpads::toggle`: нет окна → `Launch` и `take_pending_show` = true один раз; видно+фокус+на столе → `Hide`; каждое из трёх условий ложно → `Show` — тесты. `register` не перетирает живое окно; `remove_window`/`remove_app` — тесты.

**Проверка:** `cargo test` зелёный; `cargo build` без предупреждений; nightly `cargo fmt --check` чистый.

### [x] T2. sys: управление приложением

- **Зависит от:** —
- **Волна:** 1
- **Файлы (владеет):** `src/sys/app.rs`
- **Файлы (только читает):** `src/sys/bundle.rs`, `Cargo.toml` (objc2-app-kit features)
- **Скилы:** `/coding-writer`

**Что сделать.** Реализовать контракт 5.4 на `objc2`/`objc2-app-kit` (уже подключены; если нужна новая feature crate — можно дописать в `Cargo.toml`, это единственное исключение из владения, отметь в отчёте). `launch_app` не запускает shell: `std::process::Command::new("/usr/bin/open").args(["-b", id])` в отдельном потоке, результат (код выхода) логируется.

**Критерии приёмки**
- Сигнатуры ровно как в 5.4.
- `launch_app` отклоняет пустой id и id с символами вне `[A-Za-z0-9.-]` — unit-тест; валидный id не запускается в тестах (логика валидации вынесена в отдельную функцию и тестируется без запуска).
- `is_app_hidden`/`set_app_hidden`/`activate_app` возвращают `None`/`false` для несуществующего pid — тест (pid, которого нет, например `i32::MAX`).
- Нет `unwrap` на результатах Objective-C вызовов.

**Проверка:** `cargo test sys::app`, `cargo build`, nightly fmt.

### [x] T3. Разведка: перенос окна на текущий рабочий стол

- **Зависит от:** —
- **Волна:** 1
- **Файлы (владеет):** `src/sys.rs` (строка `pub mod space_move;`), `src/sys/space_move.rs` (новый), `examples/devtool.rs` (новая подкоманда), `docs/plans/research/scratchpad-space-move.md` (отчёт)
- **Файлы (только читает):** `src/sys/screen.rs`, `src/sys/window_server.rs`, исходники yabai в интернете (только чтение документации/кода для справки)
- **Скилы:** `/coding-writer` (для кода), отчёт — свободная форма

**Что сделать.** Выяснить, какой способ переносит окно чужого приложения на текущий рабочий стол на этой машине (macOS 27.0, SIP частично отключён, без загрузки scripting addition). Кандидаты:
1. `SLSMoveWindowsToManagedSpace` / `CGSMoveWindowsToManagedSpace`;
2. `SLSSpaceSetCompatID` + `SLSSetWindowListWorkspace` + `SLSSpaceSetCompatID(0)` (обходной путь yabai для новых macOS);
3. свернуть окно (AX `kAXMinimizedAttribute`) и развернуть, находясь на целевом столе;
4. приём Amethyst: зажать мышь на заголовке окна и переключить рабочий стол.

Для каждого — подкоманда `devtool` (например `devtool space move-window <wsid> <space-id>`), живая проверка и вывод. **Правила живой проверки:** работать только с окном, которое ты сам открыл (`open -a TextEdit`, новый документ), и закрыть его в конце; не трогать чужие окна; не запускать Glide; если на машине один рабочий стол (`devtool` покажет список спейсов) — не создавать новые, а вернуть BLOCKED с просьбой к человеку создать второй. Способ 4 не проверять вживую, если он требует синтетических событий мыши, — только описать.

**Критерии приёмки**
- Отчёт: по каждому способу — работает / не работает / не проверен, с точной командой и наблюдением (что вернул API, где оказалось окно по `CGSCopySpacesForWindows` или аналогу).
- Рекомендация для T6 и честная оценка: нужен ли SIP/scripting addition.
- Рабочий способ (если есть) оформлен как `move_window_to_space` в `src/sys/space_move.rs`; нерабочие не оставлены в коде.
- `cargo build --examples`, `cargo test` зелёные.

**Точка остановки:** если работает только способ, требующий scripting addition, — отчёт и BLOCKED; решение принимает человек.

### [x] R1-test. Тесты волны 1

- **Зависит от:** T1, T2, T3 · **Волна:** 1-ревью
- **Файлы (владеет):** только тестовые модули (`#[cfg(test)]`) в файлах T1/T2, `docs/plans/reviews/scratchpad-R1-test.md`
- **Скилы:** `/coding-test`

Проверить критерии приёмки T1 и T2 тестами; убедиться, что тесты падают при порче логики (`toggle`, валидация, `to_frame`, валидация bundle id).

### [x] R1-sec. Секьюрити-ревью волны 1

- **Зависит от:** T1, T2, T3 · **Волна:** 1-ревью
- **Файлы (владеет):** `docs/plans/reviews/scratchpad-R1-sec.md`
- **Скилы:** `/coding-secure`

`unsafe`/FFI в `sys/app.rs` и `sys/space_move.rs` (время жизни объектов, проверка ошибок); запуск `open -b` (нет shell, валидация id, не наследуются права Glide); значения из конфига не попадают в команды без проверки.

*R1-arch и R1-qa не нужны: волна добавляет изолированные примитивы без пользовательского поведения; архитектурная сверка — в R3.*

### [x] F1. Исправления волны 1

Владеет файлами T1–T3. Закрывает блокирующие находки R1-*. Пустая, если их нет.

### [x] T4. Поток приложения: скрытие

- **Зависит от:** T2 · **Волна:** 2
- **Файлы (владеет):** `src/actor/app.rs`, `src/actor/reactor.rs` (вариант `Event::ApplicationHiddenChanged`, флаг в `AppState`, метод `is_app_hidden`), `src/actor/reactor/testing.rs` (поддержка `Request::SetHidden` в стенде)
- **Файлы (только читает):** `src/sys/app.rs`, `src/actor/window_server.rs`
- **Скилы:** `/coding-writer`

**Что сделать.** Контракт 5.5. `Request::SetHidden(b)` вызывает `sys::app::set_app_hidden(self.pid, b)`; при ошибке — `warn!`. Уведомления скрытия/показа → `Event::ApplicationHiddenChanged`. Начальное состояние скрытости известно Reactor с момента запуска приложения.

**Критерии приёмки**
- Тест стенда Reactor: после `ApplicationHiddenChanged(pid, true)` `is_app_hidden(pid)` = true, после `false` — false; для неизвестного pid — false.
- Стенд обрабатывает `Request::SetHidden` (фиксирует запрос и отвечает `ApplicationHiddenChanged`), а не падает `todo!()`.
- Запись/воспроизведение (`Drop` в testing.rs) проходит с новым событием.
- `cargo test` зелёный.

### [x] R2-test. Тесты волны 2

- **Зависит от:** T4 · **Волна:** 2-ревью · **Владеет:** тестовые модули, `docs/plans/reviews/scratchpad-R2-test.md` · **Скилы:** `/coding-test`

*R2-sec/qa/arch не нужны: волна — внутренний канал сообщений без внешнего ввода; сверка в R3.*

### [x] F2. Исправления волны 2

### [x] T5. Интеграция в LayoutManager и Reactor

- **Зависит от:** T1, T2, T4 · **Волна:** 3
- **Файлы (владеет):** `src/actor/layout.rs`, `src/actor/reactor.rs`, `src/actor/reactor/testing.rs`, `src/model/scratchpad.rs` (только если нужно расширение — без изменения контракта 5.2)
- **Файлы (только читает):** `src/config.rs`, `src/sys/app.rs`, `src/actor/app.rs`, `src/actor/raise.rs`
- **Скилы:** `/coding-writer`

**Что сделать.**
1. LayoutManager держит `Scratchpads`. При классификации окна по правилу со `scratchpad` — окно добавляется как плавающее и регистрируется (`register`). Если для имени был отложенный показ — сразу выдаётся ответ «показать» (рамка + фокус).
2. Scratchpad-окна исключены из `toggle_focus_floating` и не становятся `last_floating_focus`.
3. Уничтожение окна / завершение приложения чистит реестр.
4. `ReactorCommand::ToggleScratchpad` в Reactor: собрать `ScratchpadWindowState` (скрыто ли приложение — `is_app_hidden`; в фокусе — главное окно; на активном столе — окно в видимых окнах активного экрана), вызвать LayoutManager, исполнить действие:
   - `Launch` → `sys::app::launch_app(launch)`; без `launch` — `warn!`;
   - `Show(wid)` → если приложение скрыто, `Request::SetHidden(false)`; `frame_overrides` = `frame.to_frame(активный экран)`; `focus_window = wid`;
   - `Hide(wid)` → `Request::SetHidden(true)`.
5. Если окно на другом рабочем столе — пока просто показать (macOS переключит стол); перенос — T6.
6. Перенесено из проверки волны 1 (обязательно):
   - фокус ставить через `focus_window` → `make_key_window`, не через `sys::app::activate_app` (T2: на macOS 14+ он может вернуть true без активации);
   - R1-sec S2: IPC `UpdateConfig` (`src/actor/server.rs`) обходит `validated()` — применять `WindowRule::validated`/`FractionalRect::validated` там, где LayoutManager берёт правила (`set_config`), чтобы путь IPC тоже был проверен;
   - R1-sec S3 + F1: `launch` и `name` у `toggle_scratchpad` проверять при загрузке конфига (пустое/пробельное имя, bundle id через `validate_bundle_id`), а не только при нажатии; ошибка — ошибка конфига;
   - R1-test #3: два правила с одним именем scratchpad — решить политику (рекомендация: ошибка конфига) и обновить тест `two_rules_with_the_same_scratchpad_name_are_accepted`;
   - снять пометку «Not functional yet» у примера `toggle_scratchpad` в `glide.default.toml` (в этом случае T5 владеет и этим файлом);
   - тест Reactor, закрепляющий заглушку T1 (`warn!`), заменить реальными.
7. Перенесено из проверки волны 2 (`docs/plans/reviews/scratchpad-R2-test.md`):
   - стенд `src/actor/reactor/testing.rs:250`: `Request::Terminate => break` при группировке запросов по pid отбрасывает уже прочитанные запросы других приложений (включая `SetHidden`) — исправить;
   - `requests()`/`tagged_requests()` не сохраняют порядок между приложениями — учитывать в тестах T5;
   - стенд отвечает `ApplicationHiddenChanged` на каждый `SetHidden`, а macOS при неизменном состоянии уведомление не шлёт — логика T5 не должна зависеть от получения ответа (показ окна не ждёт события).

**Критерии приёмки** (тесты стенда Reactor, по образцу `floating_window_restores_its_last_user_frame`)
- Окно приложения с правилом scratchpad не попадает в дерево и плавает.
- Toggle при видимом и сфокусированном окне → отправлен `SetHidden(true)`.
- Toggle при скрытом приложении → `SetHidden(false)` и окно получает рамку из `frame` на активном экране.
- Toggle без окна с `launch` → вызван запуск (запуск за швом, подменяемым в тесте); после появления окна оно получает рамку и фокус без повторного toggle.
- Toggle без окна и без `launch` → ничего не падает, `warn`.
- `toggle_focus_floating` не поднимает scratchpad-окна.
- Все прежние тесты зелёные.

### [x] R3-test, R3-arch, R3-sec, R3-qa. Проверка волны 3

- **Зависит от:** T5 · **Волна:** 3-ревью
- **Владеют:** `docs/plans/reviews/scratchpad-R3-<тип>.md`; R3-test — ещё и тестовые модули
- **Скилы:** `/coding-test`, `/coding-architect` (сверка с разделами 4–5), `/coding-secure`, `/coding-qa`

R3-arch: слои (модель без I/O), политика в LayoutManager, контракты 5.x. R3-qa: сценарии из раздела 2 по коду и тестам, краевые случаи (приложение закрыто между toggle, два окна у приложения, два правила с одним именем, Cmd+H вручную, окно на выключенном столе).

### [x] F3. Исправления волны 3

### [x] T6. Перенос окна на текущий рабочий стол (+ хвосты волны 3)

- **Зависит от:** T3, T5, F3 · **Волна:** 4
- **Файлы (владеет):** `src/sys/space_move.rs`, `src/sys/screen.rs`, `src/actor/reactor.rs`, `src/actor/reactor/testing.rs`, `src/actor/layout.rs`, `src/actor/app.rs` (только разворачивание окна), `src/actor/space_manager.rs` (только если нужно для п.5), `src/actor/window_server.rs` (Z3), `src/sys/app.rs` (Z1/Z2), `src/model/scratchpad.rs`, `glide.default.toml`
- **Файлы (только читает):** `docs/plans/research/scratchpad-space-move.md`, `docs/plans/reviews/scratchpad-R3-arch.md`, `docs/reports/F3-2026-09-16.md`
- **Скилы:** `/coding-writer`

**Что сделать.**
1. **Шов системных действий.** Вместо второго замыкания рядом с `launcher` — один подменяемый шов Reactor для системных действий: запуск приложения и перенос окна (R3-arch З3). В `Reactor::new` и в replay — заглушка без системных вызовов; настоящий — только в `Reactor::spawn`. Из второго круга R3-sec (`docs/plans/reviews/scratchpad-R3-sec-round2.md`): Z1 — таймер истечения отложенного показа ставить независимо от завершения `open` (зависший `open` не должен делать показ бессрочным), при ошибке `open` — событие сразу; Z2 — не держать поток на каждое нажатие: один таймер на имя (например `Timer::manual()` на executor), потоки — через `thread::Builder::spawn` с обработкой ошибки; Z3 — в `src/actor/window_server.rs` объединять перепроверки видимых окон по pid (флаг/множество «перепроверка запланирована», по образцу `screen_config_retry_pending`), серия `ApplicationHiddenChanged` даёт один `RecheckVisibleWindows`.
2. **Перенос в `Reactor::show_scratchpad`** (единственная точка показа для toggle и отложенного показа), до рамки и фокуса:
   - целевой стол — `space` активного экрана (`active_screen().space`);
   - признак «окно на другом столе» берётся только из записанных данных: wsid окна нет в `visible_windows`, при этом приложение не скрыто. Если приложение скрыто — переносить всё равно (перенос на тот же стол ничего не делает). Никаких `spaces_for_window` и прочих системных запросов внутри `handle_event` — это ломает replay;
   - после вызова переноса окно помечается как «ждёт переноса»; рамка и фокус выставляются, когда в `WindowsOnScreenUpdated` (записываемое событие) появится wsid окна. **Не ждать `WindowSpaceChanged`** — при переносе между столами одного экрана он не приходит (R3-arch И2);
   - таймаут ожидания — отдельным записываемым событием по образцу `ScratchpadShowExpired`/`RaiseTimeout` (не `tick_timer`); по таймауту — показать без переноса (macOS переключит стол);
   - при `Err(Unsupported)` или отсутствии active space — рамка и фокус сразу, как сейчас;
   - после переноса запросить обновление видимых окон (по образцу `RecheckVisibleWindows` из F3), иначе wsid может не появиться.
3. **Полноэкранный текущий стол.** Тип стола добавить в данные экрана через `ScreenCache` (не запрашивать SkyLight из обработчика). На полноэкранный стол не переносить — показать без переноса.
4. **`SpaceId`:** заменить `transmute` в `space_move.rs` на конструктор/геттер в `src/sys/screen.rs` (R1-sec S4). Выяснить и обосновать в отчёте, нужен ли `autoreleasepool` при вызове из потока Reactor.
5. **Окна вне активных столов (R3-test 3, R3-qa 2, R3-arch З7).** Scratchpad-окна приложений должны регистрироваться, даже если окно на выключенном или невидимом столе, чтобы toggle переносил окно к пользователю, а не запускал приложение повторно. Раскладку выключенных столов при этом не трогать. Если без глубокой переделки SpaceManager это невозможно — BLOCKED с вариантами.
6. **Свёрнутое окно (R3-qa 5).** При показе свёрнутое scratchpad-окно разворачивается (новый запрос потоку приложения, AX `kAXMinimizedAttribute`).
7. **Одна копия скрытости (R3-arch З4).** Убрать устаревающую копию `AppState.info.is_hidden` или синхронизировать её.
8. **Главное окно становится scratchpad (R3-test круг 2, находка A).** `src/actor/reactor.rs` ~1046/~1058 разбивает окна по экранам до сортировки, `src/actor/layout.rs` ~1227 берёт первое. Сортировать (главное окно, затем меньший `WindowId`) до разбиения по экранам, чтобы поведение совпадало с описанием в `glide.default.toml` ~262. Тест: главное окно на другом экране / пришло не первым.
9. **Тесты (R3-test круг 2, D и E).** `launch_app_then_does_not_call_back_for_an_invalid_id` (`src/sys/app.rs` ~410) не должен запускать настоящий `/usr/bin/open` даже при сломанной проверке id — вынести запуск за подменяемый путь. `hidden_change_schedules_a_recheck` (`src/actor/window_server.rs` ~632) не должен зависеть от реальной задержки потока (после Z3 перепроверка — через подменяемый таймер).

**Критерии приёмки** (тесты стенда Reactor, шов подменён)
- Окно на другом столе (нет в `visible_windows`, приложение не скрыто): toggle вызывает перенос на active space; рамка и фокус отправляются только после `WindowsOnScreenUpdated` с wsid окна.
- Таймаут переноса: показ без переноса; истечение старого ожидания не влияет на новое.
- `Unsupported` → рамка и фокус сразу.
- Полноэкранный активный стол → перенос не вызывается.
- Скрытое приложение: перенос + `SetHidden(false)` + рамка/фокус.
- Окно на выключенном столе регистрируется; toggle не запускает приложение повторно.
- Свёрнутое окно разворачивается при показе.
- Replay: перенос не вызывается, решения совпадают.
- Главное окно на другом экране или пришедшее вторым становится scratchpad.
- Z1: зависший `open` не продлевает показ дольше срока. Z2: число потоков не растёт с числом нажатий. Z3: серия hide/show одного pid даёт одну перепроверку.
- Все прежние тесты зелёные; `cargo build --all-targets` без warnings.

### [x] R4-test, R4-qa. Итоговая проверка и документация

- **Зависит от:** T6 (или T5, если T6 отменена) · **Волна:** 4-ревью
- **Скилы:** `/coding-test`, `/coding-qa`

R4-qa: приёмка фичи целиком по разделу 2; документация в `glide.default.toml` (правило, команда, пример ⌘⌥K для Keyguard `com.artemchep.keyguard`) и короткий раздел в `README.md`; запись в `CHANGELOG.md` не делается (её ведёт release-please). Итоговый список живых проверок для человека — на основе Ж1–Ж13 из `docs/plans/reviews/scratchpad-R3-qa.md` плюс сценарии переноса между столами.

### [x] F4. Исправления волны 4

## 9. Общая верификация

```bash
cargo build && cargo build --release
cargo test
~/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/rustfmt --edition 2024 --check $(git diff --name-only main -- '*.rs')
```
Затем (человек): установить сборку, добавить правило и хоткей для Keyguard, проверить сценарии раздела 2.

## 10. Риски и точки остановки

- **Перенос между рабочими столами** может быть невозможен на macOS 27 без scripting addition → остановка после T3.
- **Скрытие приложения прячет все его окна** — ожидаемо для выбранного способа.
- **Асинхронность запуска:** окно появляется через секунды; отложенный показ живёт до регистрации окна (без таймаута — известное упрощение).
- **Гонка ручного Cmd+H и toggle** — решается уведомлениями T4; если уведомления на macOS 27 не приходят — остановка T4 с отчётом.
- **Точки остановки:** изменение контрактов раздела 5; любые действия с scripting addition/SIP; T3 не может найти второй рабочий стол; падение существующих тестов, которое не чинится в границах задачи.
