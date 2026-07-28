# AGENTS.md — `gql_status`

Этот файл дополняет корневой `AGENTS.md` и действует для `src/gql_status/**`.
Документация и внутренние комментарии модуля должны оставаться на русском.

## Назначение и границы

`gql_status` — единая система структурированных результатов и диагностик
Ferros-Grynn. Она описывает успешное завершение, отсутствие данных,
уведомления и ошибки. Не заменяйте её строковыми ошибками, `anyhow` или
локальными error-enum подсистем.

`thiserror` не является частью архитектуры модуля. Совместимость со
`std::error::Error` реализуется явно, а источником семантики остаётся каталог
`Status`.

## Целевая модель

```text
StatusDefinition (статическая семантика кода)
            +
Status (типизированные параметры экземпляра)
            +
DiagnosticRecord (контекст события)
            │
            ▼
       StatusObject
        ├──────────────► StatusReport ─► Outcome<T>
        └── error only ► FerrosGrynnError ─► cause
```

- `StatusDefinition` хранит имя, `StatusCode`, condition, subcondition, domain,
  kind и hint.
- `StatusKind` делает допустимые сочетания явными:
  `Completion`, `Notification { classification, severity }` или
  `Error { classification, category, severity }`.
- `Status` — сгенерированный enum. Поля варианта являются параметрами сообщения
  и wire-представления.
- `DiagnosticRecord` относится к конкретному событию: location, дополнительные
  spans, timestamp, execution phase, severity override и extensions.
- `StatusObject` объединяет статус и diagnostic record и является универсальной
  публичной диагностикой.
- `StatusReport` содержит только completion/notification статусы и выбирает
  primary по GQL precedence.
- `Outcome<T>` возвращает значение вместе с неошибочными статусами.
- `FerrosGrynnError` принимает только error-kind и добавляет структурированную
  cause-цепочку.

Текущая вложенность намеренно соответствует GQL: diagnostic record является
частью status object, а не оборачивает его.

## Карта файлов

- `status.rs` — код, таксономия, определения, каталог и `StatusObject`;
- `formatting.rs` — formatter-маркеры и wire-значения параметров;
- `diagnostic.rs` — severity, execution phase и source locations;
- `error.rs` — `FerrosGrynnError`, cause и адаптеры внешних ошибок;
- `outcome.rs` — `StatusReport` и `Outcome<T>`;
- `wire.rs` — отдельный версионированный DTO;
- `crates/ferros-grynn-macros/src/status_codes.rs` — parser/codegen
  `define_status_codes!`.

При изменении DSL всегда меняйте proc-macro, каталог, compile-fail проверки и
этот документ согласованно.

## Ортогональные измерения

- `Condition` — стандартный класс результата/исключения.
- `ErrorClassification` — ожидаемая реакция клиента:
  `ClientError`, `TransientError`, `DatabaseError`.
- `NotificationClassification` — фильтрация уведомлений.
- `ErrorCategory` — стадия/характер ошибки из `gql_spec.md`:
  syntax, semantic, type, transaction, planning или runtime.
- `Domain` — подсистема-владелец: query, graph, storage, transaction, I/O,
  configuration, security или external.
- `Severity` — важность конкретной диагностики. Completion не имеет severity;
  notification допускает information/warning, error — error/critical.
- `ExecutionPhase` — динамический контекст. Он не заменяет category: например,
  runtime-ошибка может обнаружиться при constant folding в optimization.

Не возвращайте `StatusProperties`. Transient/performance выражаются
classification, а external provenance — техническим source.

## Коды

`StatusCode` отображается как `FG-XXXXX`; тело состоит ровно из пяти ASCII
цифр/заглавных букв и сохраняет семантику GQL class prefix.

- `00`, `01`, `02`, `03` — completion/warning/no data/information;
- `22` — data exception;
- `42` — syntax/access;
- `50`–`54` — processing/configuration/procedure/program limits;
- `G1`, `G2` — dependent-object/graph violations.

Для FG-specific подклассов используйте `F` в subclass, например `22F01`.
Proc-macro обязан отклонять неверный формат, дубликаты и condition, не
соответствующий class prefix. Опубликованные коды не перенумеровываются без
явной миграции протокола.

## DSL каталога

Статусы объявляются только через `define_status_codes!`:

```rust
ConversionError, "FG-22N37" => {
	kind: Error {
		classification: ErrorClassification::ClientError,
		category: ErrorCategory::Type,
		severity: Severity::Error,
	},
	condition: Condition::DataException,
	domain: Domain::Query,
	subcondition: "invalid coercion",
	message: "Cannot convert {value} to {target_type}; rejected: {value}.",
	params: {
		value: String => StringLiteral,
		target_type: String => ValueType,
	},
	hint: "Use an explicit compatible cast.",
},
```

Параметр всегда имеет независимые:

1. имя поля/именованного placeholder;
2. хранимый Rust-тип;
3. formatter-маркер.

Один placeholder может встречаться в message многократно; поле и аргумент
конструктора остаются единственными. Не создавайте `Value1`, `Value2` и
подобные семантически пустые типы.

Macro генерирует:

- вариант `Status`;
- статический `StatusDefinition`;
- message formatting и parameters map;
- snake_case constructor на `Status`;
- такой же constructor на `FerrosGrynnError` только для error-kind.

`constructor: custom_name` переопределяет имя обоих методов. Составные
предметные фабрики с несколькими cause-узлами остаются ручными в `error.rs`.

## Форматирование параметров

`ParameterFormatter<T>` пишет непосредственно в `fmt::Formatter`; не
возвращайте из него промежуточный `String`, кроме неизбежного преобразования
чужого `Display`.

Встроены `DisplayValue`, `Identifier`, `StringLiteral`, `Callable`,
`QueryParameter`, `ValueType` и `Join<Comma|And|Or, F>`. Идентификаторы и
литералы обязаны экранироваться. Join обязан корректно работать для 0/1/2/N.

Новый тип параметра должен:

- реализовать `StatusParameter` для структурированной инспекции/wire;
- иметь подходящий `ParameterFormatter<T>`;
- быть `Clone + Debug + PartialEq`, поскольку эти свойства имеет `Status`.

Не форматируйте параметр заранее в call site: это теряет структуру и мешает
другим представлениям.

## Locations

`TextSpan` использует UTF-8 byte offset/length и `u32`, как синтаксический
frontend. `Location` дополнительно хранит 1-based line/column начала и конца.

Создавайте location через `Location::from_source`: он проверяет диапазон и
границы UTF-8. Исходный текст после расчёта не сохраняется. `source_id` должен
быть безопасным логическим именем, не автоматически раскрытым путём.

Primary location попадает в стандартную wire-position. Для парных delimiter,
related expression и других пояснений используйте `LabeledLocation`.

## Ошибки и внешние источники

Cause-цепочка содержит только `FerrosGrynnError`. Внешний Rust error никогда не
должен становиться пользовательским сообщением или wire cause.

Для внешнего crate пишите явный `From<ExternalError>` или предметную фабрику:

- пользователь получает FG-код и понятный FG-template;
- оригинал можно сохранить через crate-private `with_technical_source`;
- technical source исключается из `Display`, публичного `Debug`, `chain()`,
  `std::error::Error::source()` и wire DTO;
- не используйте `external.to_string()` как клиентский message.

`chain()` идёт от контекста к корню, `root()` возвращает последний
структурированный status. Внешний status должен добавлять контекст, внутренний
cause — конкретную первопричину.

## Wire и безопасность

Не добавляйте `Serialize` непосредственно к runtime error graph.
`StatusWireV1` — отдельный versioned DTO с code, description, message,
parameters, classification/category/domain/severity, diagnostic record и
структурированным cause.

Не помещайте в message/parameters/extensions секреты, исходные страницы,
ключи, полные локальные пути или непроверенный текст внешней ошибки.
Неизвестные будущие поля размещаются в extensions.

## Проверка изменений

Для каждого статуса проверяйте:

- code/condition/kind/domain и classification/category/severity;
- message, hint и parameters;
- повтор именованного placeholder;
- escaping formatter-ов;
- `Status`- и error-конструкторы;
- wire DTO и cause.

Общие сценарии модуля:

- Unicode и многострочные locations, invalid byte boundaries;
- severity override только в допустимом семействе;
- GQL precedence в `StatusReport`;
- `Outcome<T>::map/into_parts`;
- порядок `chain()`, `root()` и `Error::source`;
- редактирование technical source.

Запускайте:

```text
cargo +nightly fmt
cargo test -p ferros-grynn-macros
cargo test gql_status
cargo check
```

Корневая crate находится в миграции и может не собираться по причинам вне
`gql_status`. Всегда отделяйте исходные failures от внесённых регрессий и
добивайтесь отсутствия ошибок, указывающих на этот модуль.
