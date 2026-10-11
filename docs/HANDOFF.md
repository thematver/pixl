# PIXL — передача работы (handoff)

Состояние на 11 октября 2026, ~12:20 МСК. Предыдущий ведущий агент остановился из-за лимита использования аккаунта. Ты подхватываешь роль **ведущего инженера**: планируешь, раздаёшь работу субагентам, сам собираешь, проверяешь и отчитываешься пользователю.

## 1. Проект и пользователь

- **PIXL** — премиальный (~$3000) chiptune / 8-bit синтезатор на Rust. Основная DAW — Logic Pro (AU), также VST3 и CLAP. Репозиторий: `/Users/anomatver/Develop/PIXL`.
- Пользователь — русскоязычный продюсер в Logic Pro. Отвечай по-русски, простым языком, без лишнего жаргона. Он требует AAA-качество и best practices. Хочет как можно раньше **играть** на инструменте. Когда просит «прогресс бар», показывай текстовые полосы по крейтам и по лимитам.
- **Обязательный контракт:** `docs/SPEC.md`, прочитай целиком. Дизайн: `docs/design/source/PIXL.dc.html` (900×560, шрифты Geist, акцент #FF5A1F) и эталонные рендеры 11 экранов `docs/design/ref/*.png` (3600×2240, масштаб 4×). Скрипт рендера эталонов — `tools/design-ref/snapshot.swift`.

## 2. Архитектура (решения уже приняты пользователем)

- nih-plug (pinned git rev `de42101`, `default-features = false`, только CLAP) → staticlib `libpixl.a` → **clap-wrapper** (CMake, режим clap-first, `wrapper/`) → AUv2 (`aumu`/`PIXL`/`PXLa`) + VST3 + CLAP.
- GUI — **нативный**, без WebView (пользователь отказался от WebView). Стек: objc2 NSView child view + wgpu 30 (Metal) + vello 0.11 + parley 0.12. Шрифты Geist лежат в `crates/pixl-gfx/assets/fonts`.
- Крейты: `pixl-core` (параметры, enum'ы, макросы, wave RAM, формат пресетов, Bridge, lock-free sync), `pixl-dsp` (движок), `pixl-fx` (эффекты), `pixl-gfx` (платформа рендера), `pixl-gui` (редактор), `pixl-plugin` (склейка nih-plug).

## 3. Сборка

- `make` — собрать Release и **установить с заменой** в `~/Library/Audio/Plug-Ins/{Components,VST3,CLAP}`. Так попросил пользователь.
- `make validate` — `auval`. `make build` — только сборка в `build/wrapper/out`. `make test` — тесты.
- `CARGO_FLAGS=--no-default-features make` — сборка **без собственного редактора**: в `pixl-plugin` фича `gui` опциональна и включена по умолчанию. Так собирать, пока `pixl-gfx`/`pixl-gui` не компилируются.
- `clap-validator` 0.4.1 уже установлен: `~/.cargo/bin/clap-validator validate build/wrapper/out/PIXL.clap`.
- Окружение: macOS, Apple M4 Max (14 ядер), rustc 1.99. Команды `timeout` нет. Системный python3 — 3.9 **без numpy**: численный анализ делать на Rust.
- Git: коммитов ещё нет, всё лежит в staged или untracked. Без просьбы пользователя не коммить, но предложи сделать первый коммит: незакоммиченной работы очень много.

## 4. Состояние по крейтам

Это последние известные данные. **Сначала проверь фактическое состояние:** субагенты могли успеть что-то изменить перед остановкой.

| Крейт | Готовность | Детали |
|---|---|---|
| pixl-core | ✅ 100% | 21 тест. Не менять без необходимости. |
| pixl-fx | ✅ 100% | 58 тестов, clippy чистый. Эхо SNES (6 FIR-пресетов Q7), crusher с minBLEP, фильтры NES/GB, ресемплер (THD+N ≈ −107…−112 dB), метры. Рендеры: `target/pixl-renders/fx/`. CPU 0.26% ядра при 44.1k. |
| pixl-dsp | ~70% | Модули analysis, blip, chip, engine, env, modulation, osc, resolve, voice, плюс `examples/render_wav.rs`, `tests/{alias,bench,engine}.rs`. Движок звучит. **Известные баги** (из e2e-тестов плагина `crates/pixl-plugin/tests/render.rs`): (1) после CC120 / AllSoundOff остаётся остаток ~0.0042; (2) падает тест `notes_end_cleanly_and_the_instrument_falls_silent`; (3) у ноты FREE-pulse в первые периоды «плавает» высота, потом стабильно (~183.4 сэмпла при 48k). Нужно также проверить полноту тестов SPEC §4.11 и замеры (алиасинг, CPU, калибровка уровней ±1.5 dB). |
| pixl-plugin | ~60% | Написаны params, state, presets, midi, processor, bridge, editor, entry, lib, `tests/{render,clap_host}.rs`, `tests/common`. Компилируется с `--no-default-features`. Шла отладка e2e-звука. **Удалить** временный `tests/diag_tmp.rs`. `clap-validator` ещё не прогонялся (проверь). |
| presets | ~50% | Генератор `tools/presets/{pixl_params,dsl,waves,library}.py` (library.py ~87 КБ). `presets/factory.json` пока заглушка на 1 пресет. Тест `crates/pixl-core/tests/factory_library.rs` не написан. |
| pixl-gfx | ~45% | Есть app, canvas, event, fonts, gpu, headless, icons, render, text, lib. **Не компилируется:** нет модуля `platform` (NSView child view — главная часть), ошибки заимствования в headless.rs и text.rs, в wgpu 30 у `RequestAdapterOptions` не хватает поля `apply_limit_buckets`. Запись `[[test]] view_lifecycle` удалена из Cargo.toml, потому что файла не было: вернуть её вместе с тестом. `examples/zz_coverage.rs` — черновик, удалить. |
| pixl-gui | 0% | Только заглушка с замороженным API: `open_editor`, `prewarm`, `DESIGN_SIZE`, `EditorHandle`. |

**Играбельная сборка** без редактора установлена в 12:16, `auval` пройден. Пользователь может играть со стандартными ручками Logic, только со стартовым пресетом.

## 5. План, согласованный с пользователем

1. **Этап 1 — доделать играбельную версию:** dsp и plugin. Исправить баги выше, дописать тесты, прогнать `clap-validator`. Затем `CARGO_FLAGS=--no-default-features make && make validate` и сообщить пользователю, что можно играть.
2. **Этап 2:** 128 пресетов (запустить генератор, валидационный тест, проверить звучание численно) и **состязательное ревью звукового тракта**: real-time safety, соответствие SPEC, точность чипов, интеграция с хостом и автоматизацией Logic. Исправить найденное, снова `make`.
3. **Этап 3:** pixl-gfx (платформа NSView, разделяемый GPU-контекст, Canvas, текст, меню, offscreen PNG) → pixl-gui (полный редактор по дизайну; снапшоты 11 состояний сверить с `docs/design/ref`). Затем обычный `make` с GUI.
4. **Финал:** `make`, `auval`, `clap-validator`, сверка интерфейса со скриншотами дизайна, независимое ревью, отчёт пользователю.

## 6. Как работать

- **Подробные задания для каждой роли** (зоны владения, требования, тесты, планка качества) уже написаны: `docs/handoff/roles-all-crates.workflow.js` (все шесть ролей) и `docs/handoff/stage1-playable.workflow.js` (продолжение dsp/fx/plugin). Используй их тексты как промпты субагентов, дополнив актуальным состоянием из раздела 4.
- **Лимиты (Team plan) — главное ограничение.** Пять параллельных агентов Opus сожгли 5-часовое окно примерно за 25 минут, два агента — примерно за час. Держи **не больше 2 агентов одновременно** и следи за лимитом. Недельный лимит уже использован примерно на 50%.
- Правила для параллельных субагентов:
  - каждый правит только свои пути;
  - субагенты не запускают `make`/`make install` с установкой и не трогают git index;
  - мелкие крейты собирают в своём `CARGO_TARGET_DIR=target/wf-<имя>`;
  - если чужой крейт временно не компилируется, ждать и повторять, не чинить чужое.
- Планка качества (SPEC §0):
  - на аудиопотоке нет аллокаций, блокировок и IO;
  - нет паник на рабочих данных, NaN/Inf не выходят наружу;
  - детерминизм;
  - rustfmt (max_width 120), clippy `-D warnings`, doc-комментарии;
  - никаких TODO-заглушек.

## 7. Первые шаги

1. Прочитай `docs/SPEC.md`, этот файл и `crates/pixl-core/src/*.rs`.
2. Проверь реальное состояние:
   - `git status`;
   - `cargo test -p pixl-core -p pixl-fx --release`;
   - `cargo test -p pixl-dsp --release`;
   - `cargo test -p pixl-plugin --release --no-default-features`;
   - `cargo check -p pixl-gfx` (ожидаемо падает).
3. Коротко сообщи пользователю состояние (можно прогресс-баром) и продолжай этап 1.
