use super::super::{
    error, mpsc, params, thread, BufRead, BufReader, Connection, DeserializeOwned, File, IndexedParallelIterator, IntoParallelIterator, MetadataError, MultiGzDecoder, ParallelIterator, Path, OPEN_LIBRARY_CHECKPOINT_RECORDS,
    PIPELINE_BUFFERED_BATCHES, PIPELINE_PARSE_RECORDS,
};
use super::core::{checkpoint_records, completed_phase};

struct RawJsonBatch {
    end_count: usize,
    finished: bool,
    records: Vec<String>,
}

struct ParsedJsonBatch<T> {
    end_count: usize,
    finished: bool,
    records: Vec<T>,
}

pub(crate) fn read_open_library_resumable<T: DeserializeOwned + Send>(
    connection: &mut Connection, path: &Path, limit: Option<usize>, phase: &str, mut import_batch: impl FnMut(&rusqlite::Transaction<'_>, Vec<T>) -> Result<(), MetadataError>,
) -> Result<usize, MetadataError> {
    read_json_resumable(connection, path, limit, phase, OPEN_LIBRARY_CHECKPOINT_RECORDS, &mut import_batch)
}

fn read_json_resumable<T: DeserializeOwned + Send>(
    connection: &mut Connection, path: &Path, limit: Option<usize>, phase: &str, batch_size: usize, import_batch: &mut impl FnMut(&rusqlite::Transaction<'_>, Vec<T>) -> Result<(), MetadataError>,
) -> Result<usize, MetadataError> {
    read_json_resumable_mapped(connection, path, limit, phase, batch_size, PIPELINE_PARSE_RECORDS, &Some, import_batch)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn read_json_resumable_mapped<T: DeserializeOwned + Send, U: Send>(
    connection: &mut Connection, path: &Path, limit: Option<usize>, phase: &str, batch_size: usize, parse_batch_size: usize, transform: &(impl Fn(T) -> Option<U> + Sync),
    import_batch: &mut impl FnMut(&rusqlite::Transaction<'_>, Vec<U>) -> Result<(), MetadataError>,
) -> Result<usize, MetadataError> {
    if let Some(records) = completed_phase(connection, phase)? {
        println!("resuming after completed {phase} phase ({records} records)");
        return usize::try_from(records).map_err(error);
    }
    let resume_at = usize::try_from(checkpoint_records(connection, phase)?).map_err(error)?;
    let path_owned = path.to_owned();
    let phase_owned = phase.to_owned();
    thread::scope(|scope| {
        let (raw_sender, raw_receiver) = mpsc::sync_channel(PIPELINE_BUFFERED_BATCHES);
        let (parsed_sender, parsed_receiver) = mpsc::sync_channel(PIPELINE_BUFFERED_BATCHES);
        let reader_path = path_owned.clone();
        let reader_phase = phase_owned.clone();
        let reader = scope.spawn(move || read_raw_json_batches(&reader_path, resume_at, limit, parse_batch_size, &reader_phase, raw_sender));
        let parser_path = path_owned.clone();
        let parser = scope.spawn(move || parse_json_batches::<T, U>(&parser_path, raw_receiver, parsed_sender, transform));

        let mut writer_result = Ok(resume_at);
        let mut last_commit = resume_at;
        let mut pending = Vec::new();
        while let Ok(batch) = parsed_receiver.recv() {
            let pending_end = batch.end_count;
            pending.extend(batch.records);
            if !batch.finished && pending_end.saturating_sub(last_commit) < batch_size {
                continue;
            }
            let result = (|| {
                let transaction = connection.transaction().map_err(error)?;
                import_batch(&transaction, std::mem::take(&mut pending))?;
                transaction
                    .execute(
                        "INSERT INTO import_checkpoint(phase,records_read,completed) VALUES(?1,?2,?3) ON CONFLICT(phase) DO UPDATE SET records_read=excluded.records_read,completed=excluded.completed",
                        params![phase, i64::try_from(pending_end).map_err(error)?, i64::from(batch.finished)],
                    )
                    .map_err(error)?;
                transaction.commit().map_err(error)?;
                last_commit = pending_end;
                if pending_end > 0 && pending_end.is_multiple_of(1_000_000) {
                    println!("read {pending_end} records from {}", path.display());
                }
                Ok(pending_end)
            })();
            match result {
                Ok(count) => writer_result = Ok(count),
                Err(failure) => {
                    writer_result = Err(failure);
                    break;
                }
            }
        }
        drop(parsed_receiver);
        let reader_result = reader.join().map_err(|_| MetadataError("dump reader thread panicked".to_owned()))?;
        let parser_result = parser.join().map_err(|_| MetadataError("JSON parser thread panicked".to_owned()))?;
        reader_result?;
        parser_result?;
        writer_result?;
        usize::try_from(checkpoint_records(connection, phase)?).map_err(error)
    })
}

fn read_raw_json_batches(path: &Path, resume_at: usize, limit: Option<usize>, batch_size: usize, phase: &str, sender: mpsc::SyncSender<RawJsonBatch>) -> Result<(), MetadataError> {
    let mut reader = BufReader::with_capacity(1024 * 1024, MultiGzDecoder::new(BufReader::with_capacity(1024 * 1024, File::open(path).map_err(error)?)));
    let mut line = String::new();
    let mut count = 0;
    while count < resume_at {
        line.clear();
        if reader.read_line(&mut line).map_err(error)? == 0 {
            return Err(MetadataError(format!("cannot resume {phase} at record {resume_at}: dump ended after {count} records")));
        }
        if open_library_json_line(&line).is_some() {
            count += 1;
        }
    }
    if resume_at > 0 {
        println!("resumed {phase} stream at record {resume_at} after replaying the compressed input");
    }
    loop {
        let mut records = Vec::with_capacity(batch_size);
        let mut finished = limit.is_some_and(|limit| count >= limit);
        while records.len() < batch_size && !finished {
            line.clear();
            if reader.read_line(&mut line).map_err(error)? == 0 {
                finished = true;
                break;
            }
            let Some(json) = open_library_json_line(&line) else { continue };
            records.push(json.to_owned());
            count += 1;
            if limit.is_some_and(|limit| count >= limit) {
                finished = true;
            }
        }
        if sender.send(RawJsonBatch { end_count: count, finished, records }).is_err() {
            return Ok(());
        }
        if finished {
            return Ok(());
        }
    }
}

fn parse_json_batches<T: DeserializeOwned + Send, U: Send>(path: &Path, receiver: mpsc::Receiver<RawJsonBatch>, sender: mpsc::SyncSender<ParsedJsonBatch<U>>, transform: &(impl Fn(T) -> Option<U> + Sync)) -> Result<(), MetadataError> {
    while let Ok(batch) = receiver.recv() {
        let first_record = batch.end_count.saturating_sub(batch.records.len());
        let records = batch
            .records
            .into_par_iter()
            .enumerate()
            .filter_map(|(index, json)| match serde_json::from_str::<T>(&json) {
                Ok(record) => transform(record),
                Err(parse_error) => {
                    if first_record + index < 10 {
                        tracing::warn!(path = %path.display(), record = first_record + index + 1, error = %parse_error, "skipping malformed dump record");
                    }
                    None
                }
            })
            .collect();
        if sender.send(ParsedJsonBatch { end_count: batch.end_count, finished: batch.finished, records }).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

fn open_library_json_line(line: &str) -> Option<&str> {
    line.splitn(5, '\t').nth(4).map(str::trim_end)
}
