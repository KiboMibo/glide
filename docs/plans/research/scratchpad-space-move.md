# Разведка T3: перенос окна чужого приложения на другой рабочий стол

- **Дата:** 2026-09-16
- **Машина:** macOS 27.0 (26A428), Apple Silicon, один дисплей
- **SIP:** Custom Configuration. Выключены Filesystem, Debugging, NVRAM и Boot-arg Restrictions. Включены Kext Signing, Kernel Integrity, DTrace, BaseSystem Verification и Authenticated Root.
- **yabai.osax:** лежит в `/Library/ScriptingAdditions`, yabai не запущен, scripting addition не загружался.
- **Итог:** рабочий способ есть, scripting addition и изменение SIP для него не нужны. Это `SLSBridgedMoveWindowsToManagedSpaceOperation` + `-performWithWMBridgeDelegate`. Он оформлен в `src/sys/space_move.rs`.

## 1. Стенд

Рабочие столы (`CGSCopyManagedDisplaySpaces`, один дисплей `37D8832A-…`):

| ManagedSpaceID | type | Что это |
|---|---|---|
| 412 | 4 (fullscreen) | WB Stream, pid 85375 |
| 4, 5, 6, 9, 7, 8, 266 | 0 (user) | обычные столы (7 шт.) |
| 430 | 4 (fullscreen) | **текущий**, kinopub (`pake`), pid 92560 |

Подопытный: TextEdit. До начала проверки он не был запущен (`pgrep -lx TextEdit` ничего не нашёл). Запуск:
`open -g -a TextEdit` и `osascript -e 'tell application "TextEdit" to make new document …'`. Получилось окно `44103` (pid 68417) на столе 4. В конце окно вернулось на стол 4 и было закрыто без сохранения (`close every document saving no`), TextEdit завершён. Других окон и приложений проверка не касалась.

**Текущий рабочий стол не переключался.** Текущий стол пользователя — полноэкранный видеоплеер (kinopub). Переключение прервало бы пользователя, поэтому окно переносилось только между скрытыми обычными столами. Одна попытка переключения всё же была: синтетическое нажатие Ctrl+Shift+← через System Events. Оно ничего не сделало, стол остался 430, и после этого я от переключения отказался.

Стол окна определялся через `SLSCopySpacesForWindows(cid, 0x7, [wid])` (команда `devtool space window <wid>`). Положение окна дополнительно сверялось через `devtool window-server get`: рамка после переносов не менялась (`867,38 857×1074`).

## 2. Результаты

| # | Способ | Результат | Доказательство |
|---|---|---|---|
| 1a | `SLSMoveWindowsToManagedSpace(cid, [wid], sid)` | **не работает** | `devtool space move-window 44103 6 --method legacy` (спайк): функция возвращает void, до/после и через 0/100/500/1000 мс окно на `[5]` |
| 1b | `-[SLSBridgedMoveWindowsToManagedSpaceOperation invokeFallback]` | **не работает** | спайк `--method fallback`: окно осталось на `[5]`. По дизассемблеру `invokeFallback` вызывает `SLSWindowServerClientMoveWindowsToManagedSpace`, то есть тот же путь, что 1a |
| 2 | `SLSSpaceSetCompatID(sid, 'yabe')` + `SLSSetWindowListWorkspace` + `SLSSpaceSetCompatID(sid, 0)` | **не работает** | спайк `--method compat`: `SLSSetWindowListWorkspace` вернула **1006** (`kCGErrorNotImplemented`). `SLSSpaceSetCompatID` вернула мусор (`1788651`), похоже, функция фактически void. Окно осталось на `[5]` |
| 3 | Свернуть через AX (`AXMinimized`) и развернуть на целевом столе | **не проверен вживую** | Окно на другом столе недоступно через AX: `set value of attribute "AXMinimized" of window "Untitled 2" …` вернула **-10006**, список окон процесса пуст. Для проверки нужен текущий обычный стол, а текущий стол — полноэкранный плеер пользователя (см. §1) |
| 4 | Amethyst: зажать мышь на заголовке и переключить стол | **не проверялся** (по условию задачи) | См. §4 |
| 5 | `SLSBridgedMoveWindowsToManagedSpaceOperation` + локальная `SLSPerformAsynchronousBridgedWindowManagementOperation` (способ yabai 7.1.25+, найденная по symtab) | **работает** | спайк `--method bridged`: адрес функции найден (`0x18b8e1c38`), окно `[4]`→`[5]` за <100 мс |
| 6 | То же, но `[[SLSWindowManagementFallbackBridge new] performAsynchronousBridgedWindowManagementOperation:op]` | **работает** | спайк `--method bridge-async`: `[5]`→`[6]` за <100 мс. С фонового потока тоже работает: `[6]`→`[7]` |
| 6' | `performSynchronousBridgedWindowManagementOperation:` | **падает** | процесс завершается с кодом 133 (SIGTRAP, внутренний assert), окно не двигается |
| **7** | **То же, но `[op performWithWMBridgeDelegate]`** | **работает — выбран** | спайк `--method delegate`: `[7]`→`[8]`; `--method delegate-thread` (из `std::thread`): `[8]`→`[266]`. Итоговый код: `devtool space move-window 44103 5` → `61ms: [SpaceId(5)]`, затем обратно на 4 |

Флаг `--method` существовал только во временной версии devtool. Способы 1–2, 5, 6 и 6' в итоговый код не вошли.

Дополнительно:
- Несуществующий стол (`move-window 44103 999999`): ошибки нет, окно остаётся на месте (`[266]`), процесс завершается с кодом 0. Ошибки о неверном столе API не возвращает.
- Для несуществующего окна `space window 999999` возвращает `[]`.
- Текущий стол после всех переносов не изменился (`Current space: SpaceId(430)`). Перенос в фоне стол не переключает.
- Вызов не зависит от `SLSMainConnectionID`, main thread ему не нужен.

## 3. Как устроен рабочий способ

В yabai это [#2788](https://github.com/koekeishiya/yabai/issues/2788), коммит `ce798b7aed` от 2026-05-08: «Moving windows between spaces works with SIP enabled again», подтверждено на macOS 26.4. В macOS 26+ SkyLight экспортирует ObjC-классы `SLSBridged*Operation`. Операция передаётся «мосту» (`SLSWMBridgeDelegate()`): это либо зарегистрированный делегат `sWindowManagementBridgeDelegate`, либо `SLSWindowManagementFallbackBridge`. Мост исполняет её в процессе, который управляет рабочими столами. Поэтому вызывающему не нужны права Dock и инъекция.

Дизассемблер (`lldb`, работал с собственным процессом devtool) показал:
- `SLSPerformAsynchronousBridgedWindowManagementOperation(op)`, которую yabai ищет по локальному символу, делает ровно `[SLSWMBridgeDelegate() performAsynchronousBridgedWindowManagementOperation:op]` и возвращает void. То же самое делает публичный метод `-[SLSAsynchronousBridgedWindowManagementOperation performWithWMBridgeDelegate]`.
- Поэтому в Glide выбран `performWithWMBridgeDelegate`. Ему не нужен разбор symtab из dyld shared cache, как у yabai, и он не обходит зарегистрированный делегат, в отличие от способа 6.

Код (`src/sys/space_move.rs`):
```rust
pub fn move_window_to_space(wsid: WindowServerId, space: SpaceId) -> Result<(), SpaceMoveError>;
pub fn spaces_for_window(wsid: WindowServerId) -> Vec<SpaceId>;
pub fn space_id_from_raw(id: u64) -> Option<SpaceId>;
```
`Err(SpaceMoveError::Unsupported)` возвращается, если класса нет (macOS < 26) или `init` вернул nil. Если перенос не удался уже после отправки, об этом не сообщается: вызов асинхронный.

devtool:
```
cargo run --example devtool -- space window <wid>
cargo run --example devtool -- space move-window <wid> <space-id>
```

## 4. Способ Amethyst (только описание)

По известному описанию (исходники Amethyst в этой задаче не перечитывались) Amethyst переносит окно так: мышь ставится на заголовок окна, синтезируется `leftMouseDown`, затем нажимается системный хоткей «перейти на рабочий стол N» (Ctrl+N), macOS «несёт» захваченное окно на новый стол, после чего синтезируется `leftMouseUp` и курсор возвращается на место. Минусы:
- пользователь видит переключение стола, а для scratchpad нужно обратное — принести окно к пользователю;
- нужны включённые хоткеи Mission Control «Switch to Desktop N» (здесь хоткеи 79/81 выключены, а синтетическое Ctrl+Shift+← не сработало);
- ненадёжно: у окна должен быть «хватаемый» заголовок, нужен тайминг, мешают полноэкранные столы;
- синтетические события мыши.
Для T6 не нужен, раз работает способ 7.

## 5. Рекомендация для T6

1. **Использовать `sys::space_move::move_window_to_space`.** SIP и scripting addition не нужны. `yabai.osax` можно не трогать. Сам вызов не опирается на выключенные части SIP: нет инъекции, нет `task_for_pid` к чужим процессам. Способ заявлен yabai как работающий «with SIP enabled». С полностью включённым SIP на этой машине он не проверялся, это остаётся без проверки.
2. Целевой стол — текущий стол экрана, на котором показывается окно (`ScreenCache::get_screen_spaces()`), а не `CGSGetActiveSpace`. Если текущий стол полноэкранный (type 4, как здесь), переносить окно туда, скорее всего, нельзя или не стоит. Поведение для этого случая T6 должен решить отдельно, например показывать окно без переноса или пропускать перенос. Проверено не было.
3. Вызов асинхронный. Порядок в T6 такой: перенести окно, дождаться `WindowSpaceChanged` / обновления экрана (или ~100 мс), затем задать рамку и фокус. Если сразу после вызова сделать `Raise`, окно может оказаться ещё на старом столе, и macOS переключит стол к нему.
4. Можно вызывать из потока Reactor: main thread не нужен.
5. `SpaceId` не имеет публичного конструктора и геттера (`new` только под `cfg(test)`). В `space_move.rs` временно используется `transmute` (корректно, т.к. `#[repr(transparent)]`). T6 стоит перенести это в `SpaceId::new/get` в `src/sys/screen.rs` — этот файл T3 было разрешено только читать.
6. Для macOS < 26, где класса нет, функция возвращает `Err(Unsupported)`. Scratchpad должен работать и без переноса (показывать окно там, где оно есть).

## 6. Что не проверено и вопросы человеку

- **Перенос на текущий видимый обычный стол.** Проверен только перенос между скрытыми столами. Появится ли окно на экране сразу, придёт ли Glide событие и не переключит ли macOS стол — не проверено: текущий стол пользователя — полноэкранный плеер, переключать его я не стал. **Просьба:** когда удобно, перейти на обычный стол, открыть TextEdit на другом столе и выполнить
  `cargo run --example devtool -- space move-window <wid> $(текущий id)` (id: `devtool list spaces`, первая строка). Или разрешить агенту переключить стол на время проверки.
- **Способ 3 (свернуть/развернуть)** не проверен по той же причине. Раз способ 7 работает, эта проверка не нужна.
- **Перенос на полноэкранный стол** намеренно не проверялся.
- Проверка шла на одном дисплее. Перенос между дисплеями не проверялся.
