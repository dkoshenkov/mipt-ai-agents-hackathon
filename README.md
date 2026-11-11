# mipt-ai-agents-hackathon

## Локальный пример проверки 223-ФЗ

```sh
cargo run --example review_223
```

Пример не требует API-ключа и не выполняет сетевых запросов. Локальные ответы моделируют находку в документе и HTTP 429 для соседней проверки. Затем пример показывает заключение критика и тайм-аут критика. В обоих случаях доступны находка, ссылки на оба фрагмента документа и ошибка проверки.

## Как добавить проверку

Добавьте в список `Check` со стабильным `id`, инструкцией в `prompt` и дополнительными материалами в `materials`. Данные ТЗ передавайте через `Document`. Инструкции проверок должны быть пригодны для отправки провайдеру. Пример возвращает ссылки на цитаты в виде байтовых диапазонов UTF-8 исходного Markdown, где конец диапазона исключается.

Сценарий можно записать обычной `async fn`. Она может собрать и вернуть отчет следующим образом.

```rust
let outcomes = agents.run_parallel(document, checks).await?;
let evidence = aggregate(document, outcomes);
let report = match critic.review(document, &evidence, &[]).await {
    Ok(verdict) => BranchReport::Reviewed { evidence, verdict },
    Err(error) => BranchReport::CriticFailed { evidence, error },
};
```

Для готовой ветки используйте `runtime::scenario::review_223`. Она связывает события этапов общим `run_id` и по умолчанию пишет JSON-журнал в stderr. Пустой список находок означает, что проверка выполнилась успешно. Отдельный сбой виден по ID проверки в `evidence.outcomes`.

## Подключение Grok

Для реальных запросов создайте `Grok` с ключом xAI и именем доступной модели и передайте его в `Runtime` вместо локальной модели. В примере конфигурация читается из окружения.

```rust
use std::time::Duration;
use procurement_review::runtime::{grok::Grok, Runtime};

fn grok_runtime() -> Result<Runtime<Grok>, std::env::VarError> {
    let api_key = std::env::var("XAI_API_KEY")?;
    let model = std::env::var("XAI_MODEL")?;
    Ok(Runtime::new(Grok::new(api_key, model), Duration::from_secs(30)))
}
```

`review_223` принимает отдельные `Runtime` для проверок и критика. Перед внешней отправкой документ и все материалы проверок должны иметь статус `Visibility::Public`.
