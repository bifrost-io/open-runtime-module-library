use crate::{Config, MomentOf, TimestampedValueOf};
use crate::{Event, Pallet};
use frame_support::traits::{Get, Time};
use orml_traits::CombineData;
use sp_runtime::traits::Saturating;
use sp_std::{marker, prelude::*};

/// Sort by value and returns median timestamped value.
/// Returns prev_value if not enough valid values.
pub struct DefaultCombineData<T, MinimumCount, ExpiresIn, MinimumTimestampInterval, MaximumValueInterval, MinimumValueInterval, I = ()>(
	marker::PhantomData<(
		T,
		I,
		MinimumCount,
		ExpiresIn,
		MinimumTimestampInterval,
		MaximumValueInterval,
		MinimumValueInterval,
	)>,
);

impl<T, I, MinimumCount, ExpiresIn, MinimumTimestampInterval, MaximumValueInterval, MinimumValueInterval>
	CombineData<<T as Config<I>>::OracleKey, TimestampedValueOf<T, I>>
	for DefaultCombineData<T, MinimumCount, ExpiresIn, MinimumTimestampInterval, MaximumValueInterval, MinimumValueInterval, I>
where
	T: Config<I>,
	I: 'static,
	MinimumCount: Get<u32>,
	ExpiresIn: Get<MomentOf<T, I>>,
	MinimumTimestampInterval: Get<MomentOf<T, I>>,
	MaximumValueInterval: Get<<T as Config<I>>::OracleValue>,
	MinimumValueInterval: Get<<T as Config<I>>::OracleValue>,
{
	fn combine_data(
		_key: &<T as Config<I>>::OracleKey,
		mut values: Vec<TimestampedValueOf<T, I>>,
		prev_value: Option<TimestampedValueOf<T, I>>,
	) -> Option<TimestampedValueOf<T, I>> {
		let expires_in = ExpiresIn::get();
		let now = T::Time::now();

		values.retain(|x| x.timestamp.saturating_add(expires_in) > now);

		let count = values.len() as u32;
		let minimum_count = MinimumCount::get();
		if count < minimum_count || count == 0 {
			return prev_value;
		}

		let mid_index = count / 2;
		// Won't panic as `values` ensured not empty.
		let (_, value, _) = values.select_nth_unstable_by(mid_index as usize, |a, b| a.value.cmp(&b.value));

		let minimum_timestamp_interval = MinimumTimestampInterval::get();
		if let Some(ref prev) = prev_value {
			if prev.timestamp.saturating_add(minimum_timestamp_interval) > now {
				log::trace!(
					target: "oracle",
					"Feed timestamp reaching limit: prev: {:?}, now: {:?}, interval: {:?}",
					prev.timestamp,
					now,
					minimum_timestamp_interval
				);
				return prev_value;
			}
		}

		let minimum_value_interval = MinimumValueInterval::get();
		let maximum_value_interval = MaximumValueInterval::get();
		if let Some(ref prev) = prev_value {
			let diff = minimum_value_interval.saturating_mul(prev.value);
			let max_diff = maximum_value_interval.saturating_mul(prev.value);
			value.value = if value.value < prev.value.saturating_sub(diff) {
				log::trace!(
					target: "oracle",
					"Feed value reaching limit: prev: {:?}, value: {:?}, diff: {:?}",
					value.value,
					prev.value,
					diff
				);
				if value.value < prev.value.saturating_sub(max_diff) {
					Pallet::<T, I>::deposit_event(Event::FeedValueReachingLimit {
						value: *value,
						prev: *prev,
					});
				}
				prev.value.saturating_sub(diff)
			} else if value.value > prev.value.saturating_add(diff) {
				log::trace!(
					target: "oracle",
					"Feed value reaching limit: prev: {:?}, value: {:?}, diff: {:?}",
					value.value,
					prev.value,
					diff
				);
				if value.value > prev.value.saturating_add(max_diff) {
					Pallet::<T, I>::deposit_event(Event::FeedValueReachingLimit {
						value: *value,
						prev: *prev,
					});
				}
				prev.value.saturating_add(diff)
			} else {
				value.value
			}
		}
		Some(value.clone())
	}
}
