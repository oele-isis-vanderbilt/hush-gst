//! `hush denoise`: plan the batch, then process files on `--jobs` threads.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::Result;
use hush_core::Settings;

use crate::cli::{DenoiseArgs, GlobalArgs};
use crate::exit;
use crate::ffmpeg;
use crate::inputs::{self, Job};
use crate::model;
use crate::pipeline::{self, Ctx};
use crate::report::{Outcome, Reporter, Status};

pub fn denoise(args: &DenoiseArgs, global: &GlobalArgs) -> Result<u8> {
    let plan = inputs::plan(&args.inputs, &inputs::Options::from(args))?;
    let mut outcomes: Vec<Outcome> = plan
        .errors
        .iter()
        .map(|e| failed(None, e.clone(), 0.0))
        .collect();

    if args.dry_run {
        for job in &plan.jobs {
            let exists = job.output.exists();
            if !args.json {
                let note = if exists && !args.overwrite {
                    "  (exists, would skip)"
                } else {
                    ""
                };
                println!("{} -> {}{note}", job.input.display(), job.output.display());
            }
            outcomes.push(Outcome {
                input: Some(job.input.clone()),
                output: Some(job.output.clone()),
                status: if exists && !args.overwrite {
                    Status::Skipped
                } else {
                    Status::Planned
                },
                error: None,
                warnings: vec![],
                duration_s: None,
                elapsed_s: 0.0,
            });
        }
        for e in &plan.errors {
            eprintln!("error: {e}");
        }
        return finish(args, &outcomes, None);
    }

    let rep = Reporter::new(plan.jobs.len(), global.quiet, global.verbose, args.json);
    for o in &outcomes {
        rep.outcome(o);
    }

    // Skip existing outputs before paying for ffmpeg and the model.
    let mut todo: Vec<Job> = Vec::new();
    for job in plan.jobs {
        if job.output.exists() && !args.overwrite {
            let o = Outcome {
                input: Some(job.input),
                output: Some(job.output),
                status: Status::Skipped,
                error: Some("output exists".into()),
                warnings: vec![],
                duration_s: None,
                elapsed_s: 0.0,
            };
            rep.outcome(&o);
            rep.tick();
            outcomes.push(o);
        } else {
            todo.push(job);
        }
    }
    if todo.is_empty() {
        return finish(args, &outcomes, Some(&rep));
    }

    let tools = ffmpeg::locate(global.ffmpeg.as_deref())?;
    rep.debug(format!(
        "ffmpeg: {} ({})",
        tools.ffmpeg.display(),
        tools.source
    ));
    let model = model::load(args.model.as_deref())?;
    rep.debug(format!(
        "model: {} ({})",
        model.manifest.name, model.manifest.sha256
    ));
    let settings = Settings::new(args.atten_lim_db)?;

    let ctx = Ctx {
        tools: &tools,
        bundle: &model.bundle,
        settings,
        raw: args.raw,
        per_channel: args.per_channel,
        overwrite: args.overwrite,
    };

    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(todo.len()));
    let workers = args.jobs().min(todo.len());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(job) = todo.get(i) else { break };
                    if pipeline::CANCEL.load(Ordering::SeqCst) {
                        break;
                    }
                    let o = one(&ctx, job, &rep);
                    rep.outcome(&o);
                    results.lock().unwrap().push((i, o));
                }
            });
        }
    });

    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    outcomes.extend(results.into_iter().map(|(_, o)| o));
    if pipeline::CANCEL.load(Ordering::SeqCst) {
        rep.error("interrupted");
    }
    finish(args, &outcomes, Some(&rep))
}

fn one(ctx: &Ctx, job: &Job, rep: &Reporter) -> Outcome {
    let start = Instant::now();
    let name = job
        .input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let res = pipeline::prepare(ctx, job).and_then(|(info, spec)| {
        let seconds = info.audio.duration.or(info.duration).unwrap_or(0.0);
        let bar = rep.file_bar(&name, seconds);
        let r = pipeline::run(ctx, job, &info, &spec, &|s| {
            bar.set_position((s * 1000.0) as u64)
        });
        rep.file_done(bar);
        r
    });
    let elapsed_s = start.elapsed().as_secs_f64();
    match res {
        Ok(done) => Outcome {
            input: Some(job.input.clone()),
            output: Some(job.output.clone()),
            status: Status::Done,
            error: None,
            warnings: done.warnings,
            duration_s: Some(done.seconds),
            elapsed_s,
        },
        Err(e) => failed(Some(job), format!("{e:#}"), elapsed_s),
    }
}

fn failed(job: Option<&Job>, error: String, elapsed_s: f64) -> Outcome {
    Outcome {
        input: job.map(|j| j.input.clone()),
        output: job.map(|j| j.output.clone()),
        status: Status::Failed,
        error: Some(error),
        warnings: vec![],
        duration_s: None,
        elapsed_s,
    }
}

fn finish(args: &DenoiseArgs, outcomes: &[Outcome], rep: Option<&Reporter>) -> Result<u8> {
    if let Some(rep) = rep {
        rep.summary(outcomes);
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(outcomes)?);
    }
    let any_failed = outcomes.iter().any(|o| o.status == Status::Failed);
    Ok(if any_failed {
        exit::FILES_FAILED
    } else {
        exit::OK
    })
}
