// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

import {BLS12381} from "../lib/bls/BLS12381.sol";

interface IBoltValidatorsV2 {
    struct ValidatorInfo {
        bytes20 pubkeyHash;
        uint32 maxCommittedGasLimit;
        address authorizedOperator;
        address controller;
    }

    struct _Validator {
        bytes20 pubkeyHash;
        uint32 maxCommittedGasLimit;
        uint32 controllerIndex;
        uint32 authorizedOperatorIndex;
    }

    error InvalidBLSSignature();
    error InvalidAuthorizedOperator();
    error ValidatorAlreadyExists();
    error ValidatorDoesNotExist();
    error UnsafeRegistrationNotAllowed();
    error UnauthorizedCaller();
    error InvalidPubkey();

    function getAllValidators() external view returns (ValidatorInfo[] memory);

    function getValidatorByPubkey(
        BLS12381.G1Point calldata pubkey
    ) external view returns (ValidatorInfo memory);

    function getValidatorByPubkeyHash(
        bytes20 pubkeyHash
    ) external view returns (ValidatorInfo memory);

    function getValidatorBySequenceNumber(
        uint32 sequenceNumber
    ) external view returns (ValidatorInfo memory);

    function registerValidatorUnsafe(
        bytes20 pubkeyHash,
        uint32 maxCommittedGasLimit,
        address authorizedOperator
    ) external;

    function registerValidator(
        BLS12381.G1Point calldata pubkey,
        BLS12381.G2Point calldata signature,
        uint32 maxCommittedGasLimit,
        address authorizedOperator
    ) external;

    function batchRegisterValidators(
        BLS12381.G1Point[] calldata pubkeys,
        BLS12381.G2Point calldata signature,
        uint32 maxCommittedGasLimit,
        address authorizedOperator
    ) external;

    function batchRegisterValidatorsUnsafe(
        bytes20[] calldata pubkeyHashes,
        uint32 maxCommittedGasLimit,
        address authorizedOperator
    ) external;

    function updateMaxCommittedGasLimit(bytes20 pubkeyHash, uint32 maxCommittedGasLimit) external;

    function hashPubkey(
        BLS12381.G1Point calldata pubkey
    ) external pure returns (bytes20);
}
