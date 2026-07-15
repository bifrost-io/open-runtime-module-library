use frame_support::traits::{
	tokens::imbalance::{ImbalanceAccounting, UnsafeConstructorDestructor, UnsafeManualAccounting},
	ExistenceRequirement, Get,
};
use parity_scale_codec::FullCodec;
use sp_runtime::{
	traits::{Convert, MaybeSerializeDeserialize, SaturatedConversion},
	DispatchError,
};
use sp_std::{
	boxed::Box,
	cmp::{Eq, PartialEq},
	fmt::Debug,
	marker::PhantomData,
	prelude::*,
	result,
};

use xcm::v5::{prelude::*, Asset, Error as XcmError, Location, Result};
use xcm_executor::{
	traits::{ConvertLocation, MatchesFungible, TransactAsset},
	AssetsInHolding,
};

use crate::UnknownAsset as UnknownAssetT;

/// Credit representation for currencies whose API does not expose a native
/// imbalance type. The actual balance accounting is performed by
/// `MultiCurrency` before this value is created or consumed.
struct MultiCurrencyCredit(u128);

impl UnsafeConstructorDestructor<u128> for MultiCurrencyCredit {
	fn unsafe_clone(&self) -> Box<dyn ImbalanceAccounting<u128>> {
		Box::new(Self(self.0))
	}

	fn forget_imbalance(&mut self) -> u128 {
		sp_std::mem::take(&mut self.0)
	}
}

impl UnsafeManualAccounting<u128> for MultiCurrencyCredit {
	fn saturating_subsume(&mut self, mut other: Box<dyn ImbalanceAccounting<u128>>) {
		self.0 = self.0.saturating_add(other.forget_imbalance());
	}
}

impl ImbalanceAccounting<u128> for MultiCurrencyCredit {
	fn amount(&self) -> u128 {
		self.0
	}

	fn saturating_take(&mut self, amount: u128) -> Box<dyn ImbalanceAccounting<u128>> {
		let taken = self.0.min(amount);
		self.0 -= taken;
		Box::new(Self(taken))
	}
}

/// Asset transaction errors.
enum Error {
	/// Failed to match fungible.
	FailedToMatchFungible,
	/// `Location` to `AccountId` Conversion failed.
	AccountIdConversionFailed,
	/// `CurrencyId` conversion failed.
	CurrencyIdConversionFailed,
}

impl From<Error> for XcmError {
	fn from(e: Error) -> Self {
		match e {
			Error::FailedToMatchFungible => XcmError::FailedToTransactAsset("FailedToMatchFungible"),
			Error::AccountIdConversionFailed => XcmError::FailedToTransactAsset("AccountIdConversionFailed"),
			Error::CurrencyIdConversionFailed => XcmError::FailedToTransactAsset("CurrencyIdConversionFailed"),
		}
	}
}

/// Deposit errors handler for `TransactAsset` implementations. Default impl for
/// `()` returns an `XcmError::FailedToTransactAsset` error.
pub trait OnDepositFail<CurrencyId, AccountId, Balance> {
	/// Called on deposit errors with a specific `currency_id`.
	fn on_deposit_currency_fail(
		err: DispatchError,
		currency_id: CurrencyId,
		who: &AccountId,
		amount: Balance,
	) -> Result;

	/// Called on unknown asset deposit errors.
	fn on_deposit_unknown_asset_fail(err: DispatchError, _asset: &Asset, _location: &Location) -> Result {
		Err(XcmError::FailedToTransactAsset(err.into()))
	}
}

impl<CurrencyId, AccountId, Balance> OnDepositFail<CurrencyId, AccountId, Balance> for () {
	fn on_deposit_currency_fail(
		err: DispatchError,
		_currency_id: CurrencyId,
		_who: &AccountId,
		_amount: Balance,
	) -> Result {
		Err(XcmError::FailedToTransactAsset(err.into()))
	}
}

/// `OnDepositFail` impl, will deposit known currencies to an alternative
/// account.
pub struct DepositToAlternative<Alternative, MultiCurrency, CurrencyId, AccountId, Balance>(
	PhantomData<(Alternative, MultiCurrency, CurrencyId, AccountId, Balance)>,
);
impl<
		Alternative: Get<AccountId>,
		MultiCurrency: orml_traits::MultiCurrency<AccountId, CurrencyId = CurrencyId, Balance = Balance>,
		AccountId: sp_std::fmt::Debug + Clone,
		CurrencyId: FullCodec + Eq + PartialEq + Copy + MaybeSerializeDeserialize + Debug,
		Balance,
	> OnDepositFail<CurrencyId, AccountId, Balance>
	for DepositToAlternative<Alternative, MultiCurrency, CurrencyId, AccountId, Balance>
{
	fn on_deposit_currency_fail(
		_err: DispatchError,
		currency_id: CurrencyId,
		_who: &AccountId,
		amount: Balance,
	) -> Result {
		MultiCurrency::deposit(currency_id, &Alternative::get(), amount)
			.map_err(|e| XcmError::FailedToTransactAsset(e.into()))
	}
}

/// The `TransactAsset` implementation, to handle `Asset` deposit/withdraw.
/// Note that teleport related functions are unimplemented.
///
/// Methods of `DepositFailureHandler` would be called on multi-currency deposit
/// errors.
///
/// If the asset is known, deposit/withdraw will be handled by `MultiCurrency`,
/// else by `UnknownAsset` if unknown.
#[allow(clippy::type_complexity)]
pub struct MultiCurrencyAdapter<
	MultiCurrency,
	UnknownAsset,
	Match,
	AccountId,
	AccountIdConvert,
	CurrencyId,
	CurrencyIdConvert,
	DepositFailureHandler,
>(
	PhantomData<(
		MultiCurrency,
		UnknownAsset,
		Match,
		AccountId,
		AccountIdConvert,
		CurrencyId,
		CurrencyIdConvert,
		DepositFailureHandler,
	)>,
);

impl<
		MultiCurrency: orml_traits::MultiCurrency<AccountId, CurrencyId = CurrencyId>,
		UnknownAsset: UnknownAssetT,
		Match: MatchesFungible<MultiCurrency::Balance>,
		AccountId: sp_std::fmt::Debug + Clone,
		AccountIdConvert: ConvertLocation<AccountId>,
		CurrencyId: FullCodec + Eq + PartialEq + Copy + MaybeSerializeDeserialize + Debug,
		CurrencyIdConvert: Convert<Asset, Option<CurrencyId>>,
		DepositFailureHandler: OnDepositFail<CurrencyId, AccountId, MultiCurrency::Balance>,
	> TransactAsset
	for MultiCurrencyAdapter<
		MultiCurrency,
		UnknownAsset,
		Match,
		AccountId,
		AccountIdConvert,
		CurrencyId,
		CurrencyIdConvert,
		DepositFailureHandler,
	>
{
	fn deposit_asset(
		mut assets: AssetsInHolding,
		location: &Location,
		_context: Option<&XcmContext>,
	) -> result::Result<(), (AssetsInHolding, XcmError)> {
		let Some(asset) = assets.assets_iter().next() else {
			return Ok(());
		};
		let result = match (
			AccountIdConvert::convert_location(location),
			CurrencyIdConvert::convert(asset.clone()),
			Match::matches_fungible(&asset),
		) {
			// known asset
			(Some(who), Some(currency_id), Some(amount)) => MultiCurrency::deposit(currency_id, &who, amount)
				.or_else(|err| DepositFailureHandler::on_deposit_currency_fail(err, currency_id, &who, amount)),
			// unknown asset
			_ => UnknownAsset::deposit(&asset, location)
				.or_else(|err| DepositFailureHandler::on_deposit_unknown_asset_fail(err, &asset, location)),
		};
		match result {
			Ok(()) => {
				// `MultiCurrency::deposit` already performed the ledger accounting.
				// Prevent the consumed holding credits from resolving a second time on drop.
				for credit in assets.fungible.values_mut() {
					credit.forget_imbalance();
				}
				Ok(())
			},
			Err(error) => Err((assets, error)),
		}
	}

	fn withdraw_asset(
		asset: &Asset,
		location: &Location,
		_maybe_context: Option<&XcmContext>,
	) -> result::Result<AssetsInHolding, XcmError> {
		UnknownAsset::withdraw(asset, location).or_else(|_| {
			let who = AccountIdConvert::convert_location(location)
				.ok_or_else(|| XcmError::from(Error::AccountIdConversionFailed))?;
			let currency_id = CurrencyIdConvert::convert(asset.clone())
				.ok_or_else(|| XcmError::from(Error::CurrencyIdConversionFailed))?;
			let amount: MultiCurrency::Balance = Match::matches_fungible(asset)
				.ok_or_else(|| XcmError::from(Error::FailedToMatchFungible))?
				.saturated_into();
			MultiCurrency::withdraw(currency_id, &who, amount, ExistenceRequirement::AllowDeath)
				.map_err(|e| XcmError::FailedToTransactAsset(e.into()))
		})?;

		let amount = match asset.fun {
			Fungible(amount) => amount,
			NonFungible(_) => return Err(XcmError::from(Error::FailedToMatchFungible)),
		};
		Ok(AssetsInHolding::new_from_fungible_credit(
			asset.id.clone(),
			Box::new(MultiCurrencyCredit(amount)),
		))
	}

	fn internal_transfer_asset(
		asset: &Asset,
		from: &Location,
		to: &Location,
		_context: &XcmContext,
	) -> result::Result<Asset, XcmError> {
		let from_account =
			AccountIdConvert::convert_location(from).ok_or_else(|| XcmError::from(Error::AccountIdConversionFailed))?;
		let to_account =
			AccountIdConvert::convert_location(to).ok_or_else(|| XcmError::from(Error::AccountIdConversionFailed))?;
		let currency_id = CurrencyIdConvert::convert(asset.clone())
			.ok_or_else(|| XcmError::from(Error::CurrencyIdConversionFailed))?;
		let amount: MultiCurrency::Balance = Match::matches_fungible(asset)
			.ok_or_else(|| XcmError::from(Error::FailedToMatchFungible))?
			.saturated_into();
		MultiCurrency::transfer(
			currency_id,
			&from_account,
			&to_account,
			amount,
			ExistenceRequirement::AllowDeath,
		)
		.map_err(|e| XcmError::FailedToTransactAsset(e.into()))?;

		Ok(asset.clone())
	}
}
