# Секьюрити-ревью волны 4 плана scratchpad (T6)

- **Дата:** 2026-09-16
- **Область:** `git diff develop...wave/scratchpad-w4 -- src glide.default.toml` (`src/sys/screen.rs`, `src/sys/space_move.rs`, `src/sys/app.rs`, `src/actor/reactor.rs`, `src/actor/reactor/system.rs`, `src/actor/reactor/testing.rs`, `src/actor/reactor/main_window.rs`, `src/actor/window_server.rs`, `src/actor/layout.rs`, `src/actor/app.rs`, `glide.default.toml`). Кроме того, прочитан код, через который идут данные: `src/sys/timer.rs` (`Timer::manual`, `set_next_fire`), `src/actor/reactor/replay.rs` (`Record::on_event`, `replay`), `src/sys/window_server.rs` (`WindowServerId::try_from`, `get_window`), `src/actor/app.rs` (`handle_request_batch`, `window_mut`, `trace`), `LayoutManager::register_scratchpad`, `classify_window`, `src/actor/server.rs` (`Request`), атрибут `minimized` в крейте `accessibility` (git-checkout `1e827c4`).
- **Документы:** план `docs/plans/2026-09-16-scratchpad.md` (T6), отчёт `docs/reports/T6-2026-09-16.md`, прошлые ревью `scratchpad-R1-sec.md`, `scratchpad-R3-sec.md`, `scratchpad-R3-sec-round2.md`.
- **Ветка:** `review/R4-sec` от `wave/scratchpad-w4` (`462774b`).
- **Инструменты:**
  - gitleaks (`gitleaks dir src`, `gitleaks dir glide.default.toml`): утечек нет.
  - `cargo test --lib -- scratchpad window_server space_move sys::app system sys::screen`: 187 passed, 0 failed.
- **Не отработало:** `cargo audit`, `osv-scanner` и `semgrep` не установлены. `Cargo.toml` и `Cargo.lock` в волне не менялись: их нет в области диффа.

## Резюме

Блокирующих и важных находок нет, есть 2 новых замечания. Z1–Z3 закрыты. S4 из R1 тоже закрыт, `transmute` удалён. У нового FFI (`SLSSpaceGetType`, `autoreleasepool`, `SpaceId::from_raw`, AX `minimized`) типы объявлены верно. Отсутствие класса или селектора SkyLight даёт `Unsupported`, а не abort. `LiveSystem` ставится только в `Reactor::spawn`, а replay и тесты работают через `NoSystem` или подделку. Перенос окна не даёт подменному приложению новых возможностей сверх принятого N2. Остаются две оговорки: номер окна для переноса сообщает само приложение, и Glide его не сверяет (Q1). Кроме того, любое приложение может бесконечно откладывать общую перепроверку видимых окон (Q2).

**Вердикт:** можно мержить. Q1 и Q2 можно закрыть в F4 или позже.

| Уровень | Кол-во |
|---|---|
| Блокирующие | 0 |
| Важные | 0 |
| Замечания | 2 |

## Статус прошлых находок

| Находка | Статус | Где проверено |
|---|---|---|
| Z1: срок показа начинается после `open` | закрыта | `system.rs:117-133`: `DelayedEvent` со сроком 30 с отправляется сразу после успешного старта потока и не зависит от завершения `open`. При `launched == false` `ScratchpadShowExpired` уходит сразу. Тесты `launch_expires_after_the_timeout_even_if_open_hangs`, `failed_open_expires_the_show_at_once` |
| Z2: поток на каждое нажатие спит до 30 с | закрыта | Сна в потоке больше нет: поток живёт, пока работает `open` (`sys/app.rs:92-106`). Создание идёт через `thread::Builder`, ошибка возвращается как `LaunchError::Spawn`, после чего Reactor пишет `error!` и снимает показ (`reactor.rs:1341-1344`), так что паники и abort нет. Сроки хранятся в `Deadlines`, не больше одной записи на ключ (`system.rs:171-194`). Остаток: если `open` зависнет, каждое нажатие оставит висеть ещё один поток и процесс `open`. Процесс на каждое нажатие запускался и до волны, а время жизни такого потока определяет только `open` |
| Z3: поток на каждое `ApplicationHiddenChanged` | закрыта | `window_server.rs:139-154,228-244`: вместо потоков один `Timer::manual()`, `recheck_pids: BTreeSet<pid_t>` и `recheck_at`. Множество ограничено числом pid и очищается при срабатывании. Тест `a_burst_of_hidden_changes_is_rechecked_once`. Об отложенной перепроверке см. Q2 |
| S1 (R1): селекторы SkyLight без проверки | закрыта (F1), не регрессировала | `space_move.rs:36-43,59-62`: `AnyClass::get` и `verify_sel` для обоих селекторов, результат кешируется в `OnceLock`, при ошибке `Unsupported` |
| S4 (R1): `transmute` для `SpaceId` | закрыта | `screen.rs:24-34`: `SpaceId::from_raw` и `get` без `unsafe`. В `space_move.rs` `transmute` не осталось. Тесты `space_id_round_trips`, `space_id_conversion_preserves_extreme_values` |
| N2 (R3): bundle id объявляет само приложение | принятый риск, перенос его не расширяет | Подробно в разделе «Перенос окна и подменное приложение» ниже |

## Находки

### Q1. Номер окна для переноса сообщает само приложение, и Glide его не сверяет

- **Серьёзность:** замечание
- **Где:** `src/actor/reactor.rs:1408-1424` (`space_move_target` берёт `window_server_id` из `self.windows`), `src/actor/reactor/system.rs:136-151` (`LiveSystem::move_window_to_space`); источник — `src/sys/app.rs:222` (`sys_id: WindowServerId::try_from(element)`), `src/sys/window_server.rs:64-76` (`_AXUIElementGetWindow`)
- **Класс:** доверие к данным другого процесса (CWE-345)
- **Кто может:** запущенное пользователем приложение, окно которого Glide зарегистрировал как scratchpad (сценарий N2: поддельный `CFBundleIdentifier`). Условие не проверено: отвечает ли на `_AXUIElementGetWindow` сам процесс приложения через свой AX-сервер. Если отвечает, то собственная реализация AX может вернуть номер чужого окна
- **Путь:** AX-элемент окна → `WindowServerId::try_from` → `WindowInfo.sys_id` → `Window.window_server_id` → toggle → `space_move_target` → `SLSBridgedMoveWindowsToManagedSpaceOperation` с этим wsid. Выполняет операцию процесс, который управляет столами, поэтому переносится окно любого владельца. Владельца окна (`kCGWindowOwnerPID`) Glide не сверяет с `wid.pid`
- **Чем грозит:** по хоткею пользователя окно другого приложения (например, документ с другого рабочего стола) переезжает на текущий стол и становится видимым. Рамку и фокус Glide при этом отправляет элементу подменного приложения, а не чужому окну. Содержимое чужого окна приложение не получает. Утечка ограничена тем, что окно видно на экране, например при демонстрации экрана. Злоумышленник, который уже выполняет код от имени пользователя, может больше, поэтому это замечание
- **Как чинить:** в `LiveSystem::move_window_to_space` перед вызовом `move_window` проверять владельца: `sys::window_server::get_window(wsid)` должен вернуть `pid == wid.pid`. Иначе возвращать ошибку (например, новый вариант `SpaceMoveError` или `Unsupported`). Проверка живёт в `LiveSystem`, поэтому replay не затрагивает: ошибка уже записывается в трассу как `ScratchpadMoveEnded`
- **Как проверить:** добавить тест `system.rs` с подменной функцией владельца: при несовпадающем pid `move_window` не вызывается, возвращается `Err`, `RecheckVisibleWindows` и `DelayedEvent` не отправляются

### Q2. Любое приложение может бесконечно откладывать общую перепроверку видимых окон

- **Серьёзность:** замечание
- **Где:** `src/actor/window_server.rs:230-234` (`recheck_at = Some(now + VISIBLE_WINDOWS_RECHECK_DELAY)` при каждом вызове)
- **Класс:** голодание обработки (CWE-400)
- **Кто может:** любое приложение, за которым следит Glide. Ему достаточно вызывать у себя `hide`/`unhide` чаще, чем раз в 250 мс. TCC-разрешения для этого не нужны (см. Z3)
- **Путь:** `ApplicationHiddenChanged` → `recheck_visible_windows` → срок общей перепроверки сдвигается на 250 мс вперёд. Пока серия идёт, `run_due_recheck` не срабатывает ни для одного pid, в том числе для pid перенесённого scratchpad-окна (`RecheckVisibleWindows` из `LiveSystem`)
- **Чем грозит:** вторая, отложенная проверка списка окон не выполняется ни для кого, пока серия не кончится. Первая, немедленная, выполняется на каждое событие. Для переноса scratchpad-окна это значит, что поздно появившийся wsid может остаться незамеченным. Тогда через 1 с придёт `ScratchpadMoveEnded`, окно покажется без ожидания, и macOS может переключить стол. Для скрытых и показанных приложений повторяется поведение до F3. Отказа в обслуживании и роста памяти нет: множество pid ограничено
- **Как чинить:** ограничить сдвиг срока: ставить срок только если его нет (`self.recheck_at.get_or_insert(now + DELAY)`), либо не позже первого запроса плюс `2 × DELAY`. Тест `a_burst_of_hidden_changes_is_rechecked_once` сейчас проверяет, что срок сдвигается, и его нужно поправить вместе с правкой
- **Как проверить:** в тесте `WindowServer` серия событий каждые 100 мс в течение 2 с даёт хотя бы одну отложенную перепроверку не позже, чем через `2 × DELAY` после первого события

## Проверенное и чистое

- **`SLSSpaceGetType` (`screen.rs:53-57,401-404`).** Объявление `fn(c_int, u64) -> c_int` совпадает с тем, как функцию объявляет yabai: `CGSSpaceID` там `uint64_t`. Вызов принимает только значения, указателей нет. Для несуществующего стола функция возвращает не 4 (тест `space_type_can_be_read` с `u64::MAX`). Символ линкуется жёстко, как и десяток других `SLS*` в `sys/window_server.rs:278-330`. Если символа на какой-то macOS нет, процесс не запустится (ошибка dyld), но посреди работы не упадёт. Эта модель риска в проекте уже принята. Вызов идёт в потоке WindowServer, а не в обработчике Reactor, и результат попадает в трассу (`ScreenSpacesChanged`).
- **`autoreleasepool` (`space_move.rs:44-54`).** В замыкании создаются только `Retained`-объекты, и все они освобождаются внутри пула. Наружу возвращается `Result` без ObjC-объектов. `?` внутри замыкания выходит из замыкания, а не мимо пула. Селекторы проверены до входа в пул.
- **AX `minimized` (`app.rs:742-747`, `sys/app.rs:276-278`).** Чтение идёт через `accessibility` 0.4 (`attribute.rs:202`: `CFBoolean`, `downcast` при несовпадении типа возвращает `Err`, а не паникует). Запись — `set_attribute(minimized, CFBoolean)`. Флаг снимается, только если окно свёрнуто, свернуть окно этот запрос не может. `window_mut` содержит `assert_eq!(wid.pid, self.pid)`, но Reactor отправляет запрос в `apps.get(&wid.pid)` (`reactor.rs:1373-1377`), так что условие выполнено. Ошибки AX проходят через `trace` и `handle_request_batch`: предупреждение в лог, без паники.
- **Шов `SystemActions`.** `grep` находит `LiveSystem::new` только в `Reactor::spawn` (`reactor.rs:418-419`) и в тестах `system.rs`, где `launch` и `move_window` подменены (`move_window` в `live_system()` паникует при вызове). `Reactor::new` ставит `NoSystem` (`reactor.rs:475`), а `replay` и `replay_trace` создают Reactor через `Reactor::new` (`replay.rs:106,148`). `NoSystem` только проверяет id и пишет в лог. Тестовый `FakeSystem` пишет в `Arc<Mutex<Vec>>`. Системных вызовов из `handle_event` не добавлено: тип стола и список окон приходят записанными событиями.
- **Запись исхода из обработчика (`reactor.rs:1397`).** `Record::on_event` без файла ничего не делает (`replay.rs:86-90`), поэтому в replay повторной записи нет. Трасса из чужого файла может содержать произвольный `ScratchpadMoveEnded(id)` или `ScreenSpacesChanged`. `NoSystem` на них ничего не делает с системой. Самое большее такое событие раньше покажет окно при воспроизведении. `write!(..).unwrap()` в `Record` был и до волны.
- **Задача сроков `run_delayed_events` (`system.rs:197-217`).** `BTreeMap` ограничен числом ключей: имена scratchpad берутся из команд хоткеев в конфиге (IPC `Request` не принимает команды Reactor, `server.rs:23-29`), окна — из переносов. Запись удаляется при срабатывании. Новая запись для того же ключа заменяет старую. Для имени это безопасно, потому что новый toggle заменяет ожидающий показ в модели. Для окна это тоже безопасно, потому что новый показ снимает старое ожидание (`pending_space_moves.remove`, `reactor.rs:1378`). Ранний или устаревший срабатывающий `Timer::manual` безвреден: `take_due` сравнивает сроки с `Instant::now()`, а цикл снова ставит таймер. Канал закрывается вместе с Reactor, и тогда цикл завершается.
- **`pending_space_moves`.** Ключи — окна, прошедшие `show_scratchpad`, то есть не больше числа зарегистрированных scratchpad-окон. Запись снимается по прибытии окна, по `ScratchpadMoveEnded`, `WindowDestroyed`, `ApplicationTerminated` и при новом показе. Оба `remove(..).unwrap()` (`reactor.rs:576` в `ScratchpadMoveEnded`, `reactor.rs:1435` в `place_moved_scratchpads`) берут ключ из той же карты непосредственно перед удалением, и между этими действиями карта не меняется. Иных новых `unwrap`/`expect` и индексации в рабочем коде нет. `screen_spaces.get(screen)?` не паникует при несовпадении длины.
- **Целевой стол.** Берётся только из `CGSManagedDisplayGetCurrentSpace` для активного экрана, то есть это стол, который пользователь сейчас видит. Внешний ввод на выбор стола не влияет. На полноэкранный стол окно не переносится (`space.fullscreen`), тест `window_is_not_moved_to_a_fullscreen_space`. При `Unsupported` или отсутствии данных окно показывается как раньше.
- **Перенос окна и подменное приложение (расширение N2).** Перенос выполняется только по нажатию хоткея пользователем или для отложенного показа после `launch` (не дольше 30 с, см. Z1). Новой возможности подменному окну он не даёт. Раньше такое окно тоже получало рамку и фокус, а macOS переключала стол к нему. Кроме того, приложение и само может показываться на всех столах (`NSWindowCollectionBehavior.canJoinAllSpaces`). Регистрация по-прежнему идёт по первому подходящему правилу. Порядок кандидатов (главное окно → видимые → меньший `WindowId`) выбирает только среди окон того же pid. Предупреждение про `app_id` в `glide.default.toml:266-269` сохранено. Об оговорке с номером окна см. Q1.
- **`ScratchpadCandidates` и выключенные столы (`layout.rs:650-660`).** Регистрируется только окно, первое подходящее правило которого содержит `scratchpad` (`scratchpad_rule`), и только если окно уже плавает или его нет в дереве и оно классифицируется как `FloatByDefault`. Окна с `layer != 0` и фантомные окна Finder получают `Untracked` и не проходят. Тайловое окно не берётся. Если правило раньше в списке задаёт окну другой класс, окно не регистрируется. Раскладка, `floating_windows` и `active_floating_windows` не меняются (тест `scratchpad_candidates_are_registered_without_layout_changes`). Действие над окном на выключенном столе происходит только по toggle пользователя с явным правилом. Так требует п.5 плана, и это описано в `glide.default.toml:190-193`. Других механизмов исключения окон или приложений, которые этот путь мог бы обойти, в конфиге нет.
- **`launch_app_with` (`sys/app.rs:92-106`).** `validate_bundle_id` вызывается до создания потока. Команда та же: `/usr/bin/open -b` с отдельными аргументами, без shell. Тесты больше не запускают настоящий `open` (`launch_with_fake_open`).
- **Логи.** Новые записи: `debug!(?wid, ?space, ?id)`, `debug!(?wid, "... {err}")`, `info!(?wid, ?space)` и `info!(bundle_id)` в `NoSystem`. Заголовков окон и пользовательских данных в них нет. `ScreenSpacesChanged` в трассе содержит только id столов и флаг полноэкранности.
- **Секреты.** gitleaks утечек не нашёл.

## План устранения

Блокирующих находок нет. Обе задачи необязательны для мержа и независимы друг от друга.

### F-Q1. Сверять владельца окна перед переносом

- **Закрывает:** Q1
- **Файлы:** `src/actor/reactor/system.rs`
- **Сделать:** в `LiveSystem` добавить подменяемую проверку владельца (по умолчанию `sys::window_server::get_window(wsid).map(|i| i.pid)`). Если владелец не совпадает с `wid.pid` или неизвестен, не вызывать `move_window` и вернуть `Err`. Reactor уже обрабатывает `Err`: показывает окно сразу и пишет исход в трассу.
- **Критерий приёмки:** добавлен тест из Q1; тесты `tests::scratchpad::space_moves` и `system::tests` зелёные.

### F-Q2. Ограничить откладывание перепроверки

- **Закрывает:** Q2
- **Файлы:** `src/actor/window_server.rs`
- **Сделать:** не сдвигать `recheck_at`, если срок уже назначен, либо ограничить сдвиг сверху.
- **Критерий приёмки:** добавлен тест из Q2; `a_burst_of_hidden_changes_is_rechecked_once` обновлён и по-прежнему проверяет, что серия даёт одну отложенную перепроверку.

## Не проверено

- **Отвечает ли на `_AXUIElementGetWindow` сам процесс приложения** и может ли он вернуть номер чужого окна. От этого зависит, реален ли Q1. Живой проверки не было.
- **Живое поведение переноса** (WM не запускался, окна не двигались по условиям задачи). Не проверены значение `SLSSpaceGetType` на полноэкранном столе (4 взято из yabai), наличие `SLSSpaceGetType` на macOS младше 26 и то, что `SLSBridgedMoveWindowsToManagedSpaceOperation` не переносит окно на системный или чужой стол при устаревшем `screen_spaces`.
- **Поведение `autoreleasepool` в фоновом потоке CFRunLoop** вживую не измерялось. Пул корректен при любом поведении run loop.
- **IPC-порт в целом** (без проверки отправителя, `UpdateConfig` с `exec`): вопрос прежний, см. R1. Нужен отдельный аудит.
- **Сканеры зависимостей** (`cargo audit`, `osv-scanner`) и `semgrep` не запускались, потому что не установлены. Зависимости в волне не менялись.
