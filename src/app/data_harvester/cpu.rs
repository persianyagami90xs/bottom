#[derive(Default, Debug, Clone)]
pub struct CpuData {
    pub cpu_prefix: String,
    pub cpu_count: Option<usize>,
    pub cpu_usage: f64,
}

pub type CpuHarvest = Vec<CpuData>;

pub type PastCpuWork = f64;
pub type PastCpuTotal = f64;

#[cfg(not(target_os = "linux"))]
use sysinfo::{ProcessorExt, System, SystemExt};

#[cfg(not(target_os = "linux"))]
pub fn get_cpu_data_list(sys: &System, show_average_cpu: bool) -> CpuHarvest {
    let cpu_data = sys.get_processors();
    let avg_cpu_usage = sys.get_global_processor_info().get_cpu_usage();
    let mut cpu_vec = vec![];

    if show_average_cpu {
        cpu_vec.push(CpuData {
            cpu_prefix: "AVG".to_string(),
            cpu_count: None,
            cpu_usage: avg_cpu_usage as f64,
        });
    }

    for (itx, cpu) in cpu_data.iter().enumerate() {
        cpu_vec.push(CpuData {
            cpu_prefix: "CPU".to_string(),
            cpu_count: Some(itx),
            cpu_usage: f64::from(cpu.get_cpu_usage()),
        });
    }

    cpu_vec
}

#[cfg(target_os = "linux")]
pub async fn get_cpu_data_list(
    show_average_cpu: bool, previous_cpu_times: &mut Vec<(PastCpuWork, PastCpuTotal)>,
    previous_average_cpu_time: &mut Option<(PastCpuWork, PastCpuTotal)>,
) -> crate::error::Result<CpuHarvest> {
    use futures::StreamExt;
    use heim::cpu::os::linux::CpuTimeExt;
    use std::collections::VecDeque;

    fn convert_cpu_times(cpu_time: &heim::cpu::CpuTime) -> (f64, f64) {
        let working_time: f64 = (cpu_time.user()
            + cpu_time.nice()
            + cpu_time.system()
            + cpu_time.irq()
            + cpu_time.soft_irq()
            + cpu_time.steal())
        .get::<heim::units::time::second>();
        (
            working_time,
            working_time
                + (cpu_time.idle() + cpu_time.io_wait()).get::<heim::units::time::second>(),
        )
    }

    fn calculate_cpu_usage_percentage(
        (previous_working_time, previous_total_time): (f64, f64),
        (current_working_time, current_total_time): (f64, f64),
    ) -> f64 {
        ((if current_working_time > previous_working_time {
            current_working_time - previous_working_time
        } else {
            0.0
        }) * 100.0)
            / (if current_total_time > previous_total_time {
                current_total_time - previous_total_time
            } else {
                1.0
            })
    }

    // Get all CPU times...
    let cpu_times = heim::cpu::times().await?;
    futures::pin_mut!(cpu_times);

    let mut cpu_deque: VecDeque<CpuData> = if previous_cpu_times.is_empty() {
        // Must initialize ourselves.  Use a very quick timeout to calculate an initial.
        futures_timer::Delay::new(std::time::Duration::from_millis(100)).await;

        let second_cpu_times = heim::cpu::times().await?;
        futures::pin_mut!(second_cpu_times);

        let mut new_cpu_times: Vec<(PastCpuWork, PastCpuTotal)> = Vec::new();
        let mut cpu_deque: VecDeque<CpuData> = VecDeque::new();
        let mut collected_zip = cpu_times.zip(second_cpu_times).enumerate(); // Gotta move it here, can't on while line.

        while let Some((itx, (past, present))) = collected_zip.next().await {
            if let (Ok(past), Ok(present)) = (past, present) {
                let present_times = convert_cpu_times(&present);
                new_cpu_times.push(present_times);
                cpu_deque.push_back(CpuData {
                    cpu_prefix: "CPU".to_string(),
                    cpu_count: Some(itx),
                    cpu_usage: calculate_cpu_usage_percentage(
                        convert_cpu_times(&past),
                        present_times,
                    ),
                });
            } else {
                new_cpu_times.push((0.0, 0.0));
                cpu_deque.push_back(CpuData {
                    cpu_prefix: "CPU".to_string(),
                    cpu_count: Some(itx),
                    cpu_usage: 0.0,
                });
            }
        }

        *previous_cpu_times = new_cpu_times;
        cpu_deque
    } else {
        let (new_cpu_times, cpu_deque): (Vec<(PastCpuWork, PastCpuTotal)>, VecDeque<CpuData>) =
            cpu_times
                .collect::<Vec<_>>()
                .await
                .iter()
                .zip(&*previous_cpu_times)
                .enumerate()
                .map(|(itx, (current_cpu, (past_cpu_work, past_cpu_total)))| {
                    if let Ok(cpu_time) = current_cpu {
                        let present_times = convert_cpu_times(&cpu_time);

                        (
                            present_times,
                            CpuData {
                                cpu_prefix: "CPU".to_string(),
                                cpu_count: Some(itx),
                                cpu_usage: calculate_cpu_usage_percentage(
                                    (*past_cpu_work, *past_cpu_total),
                                    present_times,
                                ),
                            },
                        )
                    } else {
                        (
                            (*past_cpu_work, *past_cpu_total),
                            CpuData {
                                cpu_prefix: "CPU".to_string(),
                                cpu_count: Some(itx),
                                cpu_usage: 0.0,
                            },
                        )
                    }
                })
                .unzip();

        *previous_cpu_times = new_cpu_times;
        cpu_deque
    };

    // Get average CPU if needed... and slap it at the top
    if show_average_cpu {
        let cpu_time = heim::cpu::time().await?;

        let (cpu_usage, new_average_cpu_time) = if let Some((past_cpu_work, past_cpu_total)) =
            previous_average_cpu_time
        {
            let present_times = convert_cpu_times(&cpu_time);
            (
                calculate_cpu_usage_percentage((*past_cpu_work, *past_cpu_total), present_times),
                present_times,
            )
        } else {
            // Again, we need to do a quick timeout...
            futures_timer::Delay::new(std::time::Duration::from_millis(100)).await;
            let second_cpu_time = heim::cpu::time().await?;

            let present_times = convert_cpu_times(&second_cpu_time);
            (
                calculate_cpu_usage_percentage(convert_cpu_times(&cpu_time), present_times),
                present_times,
            )
        };

        *previous_average_cpu_time = Some(new_average_cpu_time);
        cpu_deque.push_front(CpuData {
            cpu_prefix: "AVG".to_string(),
            cpu_count: None,
            cpu_usage,
        })
    }

    // Ok(Vec::from(cpu_deque.drain(0..5).collect::<Vec<_>>()))
    Ok(Vec::from(cpu_deque))
}
