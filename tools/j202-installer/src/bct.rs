//! Pure-Rust Tegra210 Boot Configuration Table (BCT) generator for `P3448_A00_lpddr4_204Mhz_P987.cfg`.
//! Exact field offsets verified byte-for-byte against L4T R32.7.6 `tegrabct`.

use crate::rcm::zero_key_cmac;

pub const BCT_SIZE: usize = 0x2800; // 10240 bytes
pub const DEFAULT_ODMDATA: u32 = 0xa4000;

const SDRAM_BASE: usize = 0x058c;
const SDRAM_STRIDE: usize = 0x768;
const SIGN_OFFSET: usize = 0x0510; // 1296
const CMAC_OFFSET: usize = 0x0310;

/// Builds an unsigned 10240-byte T210 BCT from an NVIDIA `.cfg` SDRAM file.
pub fn from_cfg_unsigned(cfg: &str, odmdata: u32) -> Result<Vec<u8>, String> {
    let mut bct = vec![0u8; BCT_SIZE];
    bct[0x0000..0x0004].copy_from_slice(&0x200u32.to_le_bytes());
    bct[0x0004..0x0008].copy_from_slice(&0x0e0fu32.to_le_bytes());
    // Customer data boot device (`sdmmc` instance 3, from `tegraparser --updatecustinfo`).
    bct[0x047c..0x0480].copy_from_slice(&3u32.to_le_bytes());
    bct[0x0508..0x050c].copy_from_slice(&odmdata.to_le_bytes());
    bct[0x0530..0x0534].copy_from_slice(&0x0021_0001u32.to_le_bytes());
    // Primary eMMC boot device parameters (`tegrabct --updatedevparam` for sdmmc instance 3).
    bct[0x0534..0x0538].copy_from_slice(&0x0eu32.to_le_bytes()); // BlockSizeLog2 = 14
    bct[0x0538..0x053c].copy_from_slice(&0x09u32.to_le_bytes()); // PageSizeLog2 = 9
    bct[0x053c..0x0540].copy_from_slice(&0x0100_0000u32.to_le_bytes()); // PartitionSize = 16 MiB
    bct[0x0540..0x0544].copy_from_slice(&1u32.to_le_bytes()); // NumParamSets = 1
    bct[0x0544..0x0548].copy_from_slice(&4u32.to_le_bytes()); // DevType[0] = Sdmmc
    bct[0x0548..0x054c].copy_from_slice(&9u32.to_le_bytes()); // Sdmmc ClockDivider = 9
    bct[0x054c..0x0550].copy_from_slice(&2u32.to_le_bytes()); // Sdmmc DataWidth = 2
    bct[0x27ec..0x27f0].copy_from_slice(&0x80u32.to_le_bytes());

    for (line_no, raw_line) in cfg.lines().enumerate() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (lhs, rhs) = line
            .split_once('=')
            .ok_or_else(|| format!("ligne BCT {} invalide : {line}", line_no + 1))?;
        let lhs = lhs.trim();
        let rhs = rhs.trim().trim_end_matches(';').trim();

        let rest = lhs
            .strip_prefix("SDRAM[")
            .ok_or_else(|| format!("champ BCT non géré ligne {} : {lhs}", line_no + 1))?;
        let (slot_str, field) = rest
            .split_once("].")
            .ok_or_else(|| format!("syntaxe SDRAM[] invalide ligne {} : {lhs}", line_no + 1))?;
        let slot: usize = slot_str
            .parse()
            .map_err(|_| format!("indice SDRAM invalide ligne {} : {slot_str}", line_no + 1))?;
        if slot >= 4 {
            return Err(format!("indice SDRAM hors limites ({slot}) ligne {}", line_no + 1));
        }

        let off =
            sdram_field_offset(field).ok_or_else(|| format!("champ SDRAM inconnu ligne {} : {field}", line_no + 1))?;
        let val = parse_value(rhs).map_err(|e| format!("valeur BCT invalide ligne {} ({rhs}) : {e}", line_no + 1))?;

        let pos = SDRAM_BASE + slot * SDRAM_STRIDE + off;
        bct[pos..pos + 4].copy_from_slice(&val.to_le_bytes());
    }

    Ok(bct)
}

/// Builds and zero-SBK signs a T210 BCT from an NVIDIA `.cfg` SDRAM file.
pub fn from_cfg(cfg: &str, odmdata: u32) -> Result<Vec<u8>, String> {
    let mut bct = from_cfg_unsigned(cfg, odmdata)?;
    let mac = zero_key_cmac(&bct[SIGN_OFFSET..]);
    bct[CMAC_OFFSET..CMAC_OFFSET + 16].copy_from_slice(&mac);
    Ok(bct)
}

fn parse_value(s: &str) -> Result<u32, String> {
    match s {
        "None"
        | "NvBootMemoryType_None"
        | "Ddr2"
        | "NvBootMemoryType_Ddr2"
        | "Ddr"
        | "NvBootMemoryType_Ddr"
        | "LpDdr"
        | "NvBootMemoryType_LpDdr" => Ok(0),
        "LpDdr2" | "NvBootMemoryType_LpDdr2" => Ok(1),
        "Ddr3" | "NvBootMemoryType_Ddr3" => Ok(2),
        "NvBootMemoryType_LpDdr4" => Ok(3),
        _ => {
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
            } else {
                s.parse::<u32>().map_err(|e| e.to_string())
            }
        }
    }
}

fn sdram_field_offset(name: &str) -> Option<usize> {
    SDRAM_FIELDS.iter().find(|&&(k, _)| k == name).map(|&(_, off)| off)
}

/// Exact `(field_name, byte_offset)` table extracted from `NvTegraT21xInitTables` in L4T `tegrabct`.
const SDRAM_FIELDS: &[(&str, usize)] = &[
    ("MemoryType", 0),
    ("PllMInputDivider", 4),
    ("PllMFeedbackDivider", 8),
    ("PllMStableTime", 12),
    ("PllMSetupControl", 16),
    ("PllMPostDivider", 20),
    ("PllMKCP", 24),
    ("PllMKVCO", 28),
    ("EmcBctSpare0", 32),
    ("EmcBctSpare1", 36),
    ("EmcBctSpare2", 40),
    ("EmcBctSpare3", 44),
    ("EmcBctSpare4", 48),
    ("EmcBctSpare5", 52),
    ("EmcBctSpare6", 56),
    ("EmcBctSpare7", 60),
    ("EmcBctSpare8", 64),
    ("EmcBctSpare9", 68),
    ("EmcBctSpare10", 72),
    ("EmcBctSpare11", 76),
    ("EmcBctSpare12", 80),
    ("EmcBctSpare13", 84),
    ("EmcClockSource", 88),
    ("EmcClockSourceDll", 92),
    ("ClkRstControllerPllmMisc2Override", 96),
    ("ClkRstControllerPllmMisc2OverrideEnable", 100),
    ("ClearClk2Mc1", 104),
    ("EmcAutoCalInterval", 108),
    ("EmcAutoCalConfig", 112),
    ("EmcAutoCalConfig2", 116),
    ("EmcAutoCalConfig3", 120),
    ("EmcAutoCalConfig4", 124),
    ("EmcAutoCalConfig5", 128),
    ("EmcAutoCalConfig6", 132),
    ("EmcAutoCalConfig7", 136),
    ("EmcAutoCalConfig8", 140),
    ("EmcAutoCalVrefSel0", 144),
    ("EmcAutoCalVrefSel1", 148),
    ("EmcAutoCalChannel", 152),
    ("EmcPmacroAutocalCfg0", 156),
    ("EmcPmacroAutocalCfg1", 160),
    ("EmcPmacroAutocalCfg2", 164),
    ("EmcPmacroRxTerm", 168),
    ("EmcPmacroDqTxDrv", 172),
    ("EmcPmacroCaTxDrv", 176),
    ("EmcPmacroCmdTxDrv", 180),
    ("EmcPmacroAutocalCfgCommon", 184),
    ("EmcPmacroZctrl", 188),
    ("EmcAutoCalWait", 192),
    ("EmcXm2CompPadCtrl", 196),
    ("EmcXm2CompPadCtrl2", 200),
    ("EmcXm2CompPadCtrl3", 204),
    ("EmcAdrCfg", 208),
    ("EmcPinGpioEn", 220),
    ("EmcPinGpio", 224),
    ("EmcPinProgramWait", 212),
    ("EmcPinExtraWait", 216),
    ("EmcTimingControlWait", 228),
    ("EmcRc", 232),
    ("EmcRfc", 236),
    ("EmcRfcPb", 240),
    ("EmcRefctrl2", 244),
    ("EmcRfcSlr", 248),
    ("EmcRas", 252),
    ("EmcRp", 256),
    ("EmcR2r", 260),
    ("EmcW2w", 264),
    ("EmcR2w", 268),
    ("EmcW2r", 272),
    ("EmcR2p", 276),
    ("EmcW2p", 280),
    ("EmcTppd", 284),
    ("EmcCcdmw", 288),
    ("EmcRdRcd", 292),
    ("EmcWrRcd", 296),
    ("EmcRrd", 300),
    ("EmcRext", 304),
    ("EmcWext", 308),
    ("EmcWdv", 312),
    ("EmcWdvChk", 316),
    ("EmcWsv", 320),
    ("EmcWev", 324),
    ("EmcWdvMask", 328),
    ("EmcWsDuration", 332),
    ("EmcWeDuration", 336),
    ("EmcQUse", 340),
    ("EmcQuseWidth", 344),
    ("EmcIbdly", 348),
    ("EmcObdly", 352),
    ("EmcEInput", 356),
    ("EmcEInputDuration", 360),
    ("EmcPutermExtra", 364),
    ("EmcPutermWidth", 368),
    ("EmcQRst", 372),
    ("EmcQSafe", 376),
    ("EmcRdv", 380),
    ("EmcRdvMask", 384),
    ("EmcRdvEarly", 388),
    ("EmcRdvEarlyMask", 392),
    ("EmcQpop", 396),
    ("EmcRefresh", 400),
    ("EmcBurstRefreshNum", 404),
    ("EmcPreRefreshReqCnt", 408),
    ("EmcPdEx2Wr", 412),
    ("EmcPdEx2Rd", 416),
    ("EmcPChg2Pden", 420),
    ("EmcAct2Pden", 424),
    ("EmcAr2Pden", 428),
    ("EmcRw2Pden", 432),
    ("EmcCke2Pden", 436),
    ("EmcPdex2Cke", 440),
    ("EmcPdex2Mrr", 444),
    ("EmcTxsr", 448),
    ("EmcTxsrDll", 452),
    ("EmcTcke", 456),
    ("EmcTckesr", 460),
    ("EmcTpd", 464),
    ("EmcTfaw", 468),
    ("EmcTrpab", 472),
    ("EmcTClkStable", 476),
    ("EmcTClkStop", 480),
    ("EmcTRefBw", 484),
    ("EmcFbioCfg5", 488),
    ("EmcFbioCfg7", 492),
    ("EmcFbioCfg8", 496),
    ("EmcCmdMappingCmd0_0", 500),
    ("EmcCmdMappingCmd0_1", 504),
    ("EmcCmdMappingCmd0_2", 508),
    ("EmcCmdMappingCmd1_0", 512),
    ("EmcCmdMappingCmd1_1", 516),
    ("EmcCmdMappingCmd1_2", 520),
    ("EmcCmdMappingCmd2_0", 524),
    ("EmcCmdMappingCmd2_1", 528),
    ("EmcCmdMappingCmd2_2", 532),
    ("EmcCmdMappingCmd3_0", 536),
    ("EmcCmdMappingCmd3_1", 540),
    ("EmcCmdMappingCmd3_2", 544),
    ("EmcCmdMappingByte", 548),
    ("EmcFbioSpare", 552),
    ("EmcCfgRsv", 556),
    ("EmcMrs", 560),
    ("EmcEmrs", 564),
    ("EmcEmrs2", 568),
    ("EmcEmrs3", 572),
    ("EmcMrw1", 576),
    ("EmcMrw2", 580),
    ("EmcMrw3", 584),
    ("EmcMrw4", 588),
    ("EmcMrw6", 592),
    ("EmcMrw8", 596),
    ("EmcMrw9", 600),
    ("EmcMrw10", 604),
    ("EmcMrw12", 608),
    ("EmcMrw13", 612),
    ("EmcMrw14", 616),
    ("EmcMrwExtra", 620),
    ("EmcWarmBootMrwExtra", 624),
    ("EmcWarmBootExtraModeRegWriteEnable", 628),
    ("EmcExtraModeRegWriteEnable", 632),
    ("EmcMrwResetCommand", 636),
    ("EmcMrwResetNInitWait", 640),
    ("EmcMrsWaitCnt", 644),
    ("EmcMrsWaitCnt2", 648),
    ("EmcCfg", 652),
    ("EmcCfg2", 656),
    ("EmcCfgPipe", 660),
    ("EmcCfgPipeClk", 664),
    ("EmcFdpdCtrlCmdNoRamp", 668),
    ("EmcCfgUpdate", 672),
    ("EmcDbg", 676),
    ("EmcDbgWriteMux", 680),
    ("EmcCmdQ", 684),
    ("EmcMc2EmcQ", 688),
    ("EmcDynSelfRefControl", 692),
    ("AhbArbitrationXbarCtrlMemInitDone", 696),
    ("EmcCfgDigDll", 700),
    ("EmcCfgDigDll_1", 704),
    ("EmcCfgDigDllPeriod", 708),
    ("EmcDevSelect", 712),
    ("EmcSelDpdCtrl", 716),
    ("EmcFdpdCtrlDq", 720),
    ("EmcFdpdCtrlCmd", 724),
    ("EmcPmacroIbVrefDq_0", 728),
    ("EmcPmacroIbVrefDq_1", 732),
    ("EmcPmacroIbVrefDqs_0", 736),
    ("EmcPmacroIbVrefDqs_1", 740),
    ("EmcPmacroIbRxrt", 744),
    ("EmcCfgPipe1", 748),
    ("EmcCfgPipe2", 752),
    ("EmcPmacroQuseDdllRank0_0", 756),
    ("EmcPmacroQuseDdllRank0_1", 760),
    ("EmcPmacroQuseDdllRank0_2", 764),
    ("EmcPmacroQuseDdllRank0_3", 768),
    ("EmcPmacroQuseDdllRank0_4", 772),
    ("EmcPmacroQuseDdllRank0_5", 776),
    ("EmcPmacroQuseDdllRank1_0", 780),
    ("EmcPmacroQuseDdllRank1_1", 784),
    ("EmcPmacroQuseDdllRank1_2", 788),
    ("EmcPmacroQuseDdllRank1_3", 792),
    ("EmcPmacroQuseDdllRank1_4", 796),
    ("EmcPmacroQuseDdllRank1_5", 800),
    ("EmcPmacroObDdllLongDqRank0_0", 804),
    ("EmcPmacroObDdllLongDqRank0_1", 808),
    ("EmcPmacroObDdllLongDqRank0_2", 812),
    ("EmcPmacroObDdllLongDqRank0_3", 816),
    ("EmcPmacroObDdllLongDqRank0_4", 820),
    ("EmcPmacroObDdllLongDqRank0_5", 824),
    ("EmcPmacroObDdllLongDqRank1_0", 828),
    ("EmcPmacroObDdllLongDqRank1_1", 832),
    ("EmcPmacroObDdllLongDqRank1_2", 836),
    ("EmcPmacroObDdllLongDqRank1_3", 840),
    ("EmcPmacroObDdllLongDqRank1_4", 844),
    ("EmcPmacroObDdllLongDqRank1_5", 848),
    ("EmcPmacroObDdllLongDqsRank0_0", 852),
    ("EmcPmacroObDdllLongDqsRank0_1", 856),
    ("EmcPmacroObDdllLongDqsRank0_2", 860),
    ("EmcPmacroObDdllLongDqsRank0_3", 864),
    ("EmcPmacroObDdllLongDqsRank0_4", 868),
    ("EmcPmacroObDdllLongDqsRank0_5", 872),
    ("EmcPmacroObDdllLongDqsRank1_0", 876),
    ("EmcPmacroObDdllLongDqsRank1_1", 880),
    ("EmcPmacroObDdllLongDqsRank1_2", 884),
    ("EmcPmacroObDdllLongDqsRank1_3", 888),
    ("EmcPmacroObDdllLongDqsRank1_4", 892),
    ("EmcPmacroObDdllLongDqsRank1_5", 896),
    ("EmcPmacroIbDdllLongDqsRank0_0", 900),
    ("EmcPmacroIbDdllLongDqsRank0_1", 904),
    ("EmcPmacroIbDdllLongDqsRank0_2", 908),
    ("EmcPmacroIbDdllLongDqsRank0_3", 912),
    ("EmcPmacroIbDdllLongDqsRank1_0", 916),
    ("EmcPmacroIbDdllLongDqsRank1_1", 920),
    ("EmcPmacroIbDdllLongDqsRank1_2", 924),
    ("EmcPmacroIbDdllLongDqsRank1_3", 928),
    ("EmcPmacroDdllLongCmd_0", 932),
    ("EmcPmacroDdllLongCmd_1", 936),
    ("EmcPmacroDdllLongCmd_2", 940),
    ("EmcPmacroDdllLongCmd_3", 944),
    ("EmcPmacroDdllLongCmd_4", 948),
    ("EmcPmacroDdllShortCmd_0", 952),
    ("EmcPmacroDdllShortCmd_1", 956),
    ("EmcPmacroDdllShortCmd_2", 960),
    ("WarmBootWait", 964),
    ("EmcOdtWrite", 968),
    ("EmcZcalInterval", 972),
    ("EmcZcalWaitCnt", 976),
    ("EmcZcalMrwCmd", 980),
    ("EmcMrsResetDll", 984),
    ("EmcZcalInitDev0", 988),
    ("EmcZcalInitDev1", 992),
    ("EmcZcalInitWait", 996),
    ("EmcZcalWarmColdBootEnables", 1000),
    ("EmcMrwLpddr2ZcalWarmBoot", 1004),
    ("EmcZqCalDdr3WarmBoot", 1008),
    ("EmcZqCalLpDdr4WarmBoot", 1012),
    ("EmcZcalWarmBootWait", 1016),
    ("EmcMrsWarmBootEnable", 1020),
    ("EmcMrsResetDllWait", 1024),
    ("EmcMrsExtra", 1028),
    ("EmcWarmBootMrsExtra", 1032),
    ("EmcEmrsDdr2DllEnable", 1036),
    ("EmcMrsDdr2DllReset", 1040),
    ("EmcEmrsDdr2OcdCalib", 1044),
    ("EmcDdr2Wait", 1048),
    ("EmcClkenOverride", 1052),
    ("EmcExtraRefreshNum", 1056),
    ("EmcClkenOverrideAllWarmBoot", 1060),
    ("McClkenOverrideAllWarmBoot", 1064),
    ("EmcCfgDigDllPeriodWarmBoot", 1068),
    ("PmcVddpSel", 1072),
    ("PmcVddpSelWait", 1076),
    ("PmcDdrPwr", 1080),
    ("PmcDdrCfg", 1084),
    ("PmcIoDpd3Req", 1088),
    ("PmcIoDpd3ReqWait", 1092),
    ("PmcIoDpd4ReqWait", 1096),
    ("PmcRegShort", 1100),
    ("PmcNoIoPower", 1104),
    ("PmcDdrCntrlWait", 1108),
    ("PmcDdrCntrl", 1112),
    ("EmcAcpdControl", 1116),
    ("EmcSwizzleRank0Byte0", 1120),
    ("EmcSwizzleRank0Byte1", 1124),
    ("EmcSwizzleRank0Byte2", 1128),
    ("EmcSwizzleRank0Byte3", 1132),
    ("EmcSwizzleRank1Byte0", 1136),
    ("EmcSwizzleRank1Byte1", 1140),
    ("EmcSwizzleRank1Byte2", 1144),
    ("EmcSwizzleRank1Byte3", 1148),
    ("EmcTxdsrvttgen", 1152),
    ("EmcDataBrlshft0", 1156),
    ("EmcDataBrlshft1", 1160),
    ("EmcDqsBrlshft0", 1164),
    ("EmcDqsBrlshft1", 1168),
    ("EmcCmdBrlshft0", 1172),
    ("EmcCmdBrlshft1", 1176),
    ("EmcCmdBrlshft2", 1180),
    ("EmcCmdBrlshft3", 1184),
    ("EmcQuseBrlshft0", 1188),
    ("EmcQuseBrlshft1", 1192),
    ("EmcQuseBrlshft2", 1196),
    ("EmcQuseBrlshft3", 1200),
    ("EmcDllCfg0", 1204),
    ("EmcDllCfg1", 1208),
    ("EmcPmcScratch1", 1212),
    ("EmcPmcScratch2", 1216),
    ("EmcPmcScratch3", 1220),
    ("EmcPmacroPadCfgCtrl", 1224),
    ("EmcPmacroVttgenCtrl0", 1228),
    ("EmcPmacroVttgenCtrl1", 1232),
    ("EmcPmacroVttgenCtrl2", 1236),
    ("EmcPmacroBrickCtrlRfu1", 1240),
    ("EmcPmacroCmdBrickCtrlFdpd", 1244),
    ("EmcPmacroBrickCtrlRfu2", 1248),
    ("EmcPmacroDataBrickCtrlFdpd", 1252),
    ("EmcPmacroBgBiasCtrl0", 1256),
    ("EmcPmacroDataPadRxCtrl", 1260),
    ("EmcPmacroCmdPadRxCtrl", 1264),
    ("EmcPmacroDataRxTermMode", 1268),
    ("EmcPmacroCmdRxTermMode", 1272),
    ("EmcPmacroDataPadTxCtrl", 1276),
    ("EmcPmacroCommonPadTxCtrl", 1280),
    ("EmcPmacroCmdPadTxCtrl", 1284),
    ("EmcCfg3", 1288),
    ("EmcPmacroTxPwrd0", 1292),
    ("EmcPmacroTxPwrd1", 1296),
    ("EmcPmacroTxPwrd2", 1300),
    ("EmcPmacroTxPwrd3", 1304),
    ("EmcPmacroTxPwrd4", 1308),
    ("EmcPmacroTxPwrd5", 1312),
    ("EmcConfigSampleDelay", 1316),
    ("EmcPmacroBrickMapping0", 1320),
    ("EmcPmacroBrickMapping1", 1324),
    ("EmcPmacroBrickMapping2", 1328),
    ("EmcPmacroTxSelClkSrc0", 1332),
    ("EmcPmacroTxSelClkSrc1", 1336),
    ("EmcPmacroTxSelClkSrc2", 1340),
    ("EmcPmacroTxSelClkSrc3", 1344),
    ("EmcPmacroTxSelClkSrc4", 1348),
    ("EmcPmacroTxSelClkSrc5", 1352),
    ("EmcPmacroDdllBypass", 1356),
    ("EmcPmacroDdllPwrd0", 1360),
    ("EmcPmacroDdllPwrd1", 1364),
    ("EmcPmacroDdllPwrd2", 1368),
    ("EmcPmacroCmdCtrl0", 1372),
    ("EmcPmacroCmdCtrl1", 1376),
    ("EmcPmacroCmdCtrl2", 1380),
    ("McEmemAdrCfg", 1384),
    ("McEmemAdrCfgDev0", 1388),
    ("McEmemAdrCfgDev1", 1392),
    ("McEmemAdrCfgChannelMask", 1396),
    ("McEmemAdrCfgBankMask0", 1400),
    ("McEmemAdrCfgBankMask1", 1404),
    ("McEmemAdrCfgBankMask2", 1408),
    ("McEmemCfg", 1412),
    ("McEmemArbCfg", 1416),
    ("McEmemArbOutstandingReq", 1420),
    ("McEmemArbRefpbHpCtrl", 1424),
    ("McEmemArbRefpbBankCtrl", 1428),
    ("McEmemArbTimingRcd", 1432),
    ("McEmemArbTimingRp", 1436),
    ("McEmemArbTimingRc", 1440),
    ("McEmemArbTimingRas", 1444),
    ("McEmemArbTimingFaw", 1448),
    ("McEmemArbTimingRrd", 1452),
    ("McEmemArbTimingRap2Pre", 1456),
    ("McEmemArbTimingWap2Pre", 1460),
    ("McEmemArbTimingR2R", 1464),
    ("McEmemArbTimingW2W", 1468),
    ("McEmemArbTimingR2W", 1472),
    ("McEmemArbTimingW2R", 1476),
    ("McEmemArbTimingRFCPB", 1480),
    ("McEmemArbDaTurns", 1484),
    ("McEmemArbDaCovers", 1488),
    ("McEmemArbMisc0", 1492),
    ("McEmemArbMisc1", 1496),
    ("McEmemArbMisc2", 1500),
    ("McEmemArbRing1Throttle", 1504),
    ("McEmemArbOverride", 1508),
    ("McEmemArbOverride1", 1512),
    ("McEmemArbRsv", 1516),
    ("McDaCfg0", 1520),
    ("McEmemArbTimingCcdmw", 1524),
    ("McClkenOverride", 1528),
    ("McStatControl", 1532),
    ("McVideoProtectBom", 1536),
    ("McVideoProtectBomAdrHi", 1540),
    ("McVideoProtectSizeMb", 1544),
    ("McVideoProtectVprOverride", 1548),
    ("McVideoProtectVprOverride1", 1552),
    ("McVideoProtectGpuOverride0", 1556),
    ("McVideoProtectGpuOverride1", 1560),
    ("McSecCarveoutBom", 1564),
    ("McSecCarveoutAdrHi", 1568),
    ("McSecCarveoutSizeMb", 1572),
    ("McVideoProtectWriteAccess", 1576),
    ("McSecCarveoutProtectWriteAccess", 1580),
    ("McGeneralizedCarveout1Bom", 1584),
    ("McGeneralizedCarveout1BomHi", 1588),
    ("McGeneralizedCarveout1Size128kb", 1592),
    ("McGeneralizedCarveout1Access0", 1596),
    ("McGeneralizedCarveout1Access1", 1600),
    ("McGeneralizedCarveout1Access2", 1604),
    ("McGeneralizedCarveout1Access3", 1608),
    ("McGeneralizedCarveout1Access4", 1612),
    ("McGeneralizedCarveout1ForceInternalAccess0", 1616),
    ("McGeneralizedCarveout1ForceInternalAccess1", 1620),
    ("McGeneralizedCarveout1ForceInternalAccess2", 1624),
    ("McGeneralizedCarveout1ForceInternalAccess3", 1628),
    ("McGeneralizedCarveout1ForceInternalAccess4", 1632),
    ("McGeneralizedCarveout1Cfg0", 1636),
    ("McGeneralizedCarveout2Bom", 1640),
    ("McGeneralizedCarveout2BomHi", 1644),
    ("McGeneralizedCarveout2Size128kb", 1648),
    ("McGeneralizedCarveout2Access0", 1652),
    ("McGeneralizedCarveout2Access1", 1656),
    ("McGeneralizedCarveout2Access2", 1660),
    ("McGeneralizedCarveout2Access3", 1664),
    ("McGeneralizedCarveout2Access4", 1668),
    ("McGeneralizedCarveout2ForceInternalAccess0", 1672),
    ("McGeneralizedCarveout2ForceInternalAccess1", 1676),
    ("McGeneralizedCarveout2ForceInternalAccess2", 1680),
    ("McGeneralizedCarveout2ForceInternalAccess3", 1684),
    ("McGeneralizedCarveout2ForceInternalAccess4", 1688),
    ("McGeneralizedCarveout2Cfg0", 1692),
    ("McGeneralizedCarveout3Bom", 1696),
    ("McGeneralizedCarveout3BomHi", 1700),
    ("McGeneralizedCarveout3Size128kb", 1704),
    ("McGeneralizedCarveout3Access0", 1708),
    ("McGeneralizedCarveout3Access1", 1712),
    ("McGeneralizedCarveout3Access2", 1716),
    ("McGeneralizedCarveout3Access3", 1720),
    ("McGeneralizedCarveout3Access4", 1724),
    ("McGeneralizedCarveout3ForceInternalAccess0", 1728),
    ("McGeneralizedCarveout3ForceInternalAccess1", 1732),
    ("McGeneralizedCarveout3ForceInternalAccess2", 1736),
    ("McGeneralizedCarveout3ForceInternalAccess3", 1740),
    ("McGeneralizedCarveout3ForceInternalAccess4", 1744),
    ("McGeneralizedCarveout3Cfg0", 1748),
    ("McGeneralizedCarveout4Bom", 1752),
    ("McGeneralizedCarveout4BomHi", 1756),
    ("McGeneralizedCarveout4Size128kb", 1760),
    ("McGeneralizedCarveout4Access0", 1764),
    ("McGeneralizedCarveout4Access1", 1768),
    ("McGeneralizedCarveout4Access2", 1772),
    ("McGeneralizedCarveout4Access3", 1776),
    ("McGeneralizedCarveout4Access4", 1780),
    ("McGeneralizedCarveout4ForceInternalAccess0", 1784),
    ("McGeneralizedCarveout4ForceInternalAccess1", 1788),
    ("McGeneralizedCarveout4ForceInternalAccess2", 1792),
    ("McGeneralizedCarveout4ForceInternalAccess3", 1796),
    ("McGeneralizedCarveout4ForceInternalAccess4", 1800),
    ("McGeneralizedCarveout4Cfg0", 1804),
    ("McGeneralizedCarveout5Bom", 1808),
    ("McGeneralizedCarveout5BomHi", 1812),
    ("McGeneralizedCarveout5Size128kb", 1816),
    ("McGeneralizedCarveout5Access0", 1820),
    ("McGeneralizedCarveout5Access1", 1824),
    ("McGeneralizedCarveout5Access2", 1828),
    ("McGeneralizedCarveout5Access3", 1832),
    ("McGeneralizedCarveout5Access4", 1836),
    ("McGeneralizedCarveout5ForceInternalAccess0", 1840),
    ("EmcCaTrainingEnable", 1864),
    ("McGeneralizedCarveout5ForceInternalAccess1", 1844),
    ("McGeneralizedCarveout5ForceInternalAccess2", 1848),
    ("McGeneralizedCarveout5ForceInternalAccess3", 1852),
    ("McGeneralizedCarveout5ForceInternalAccess4", 1856),
    ("McGeneralizedCarveout5Cfg0", 1860),
    ("SwizzleRankByteEncode", 1868),
    ("BootRomPatchControl", 1872),
    ("BootRomPatchData", 1876),
    ("McMtsCarveoutBom", 1880),
    ("McMtsCarveoutAdrHi", 1884),
    ("McMtsCarveoutSizeMb", 1888),
    ("McMtsCarveoutRegCtrl", 1892),
];

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn builds_minimal_bct_header_and_cmac() {
        let cfg = "\
# comment
SDRAM[0].MemoryType = NvBootMemoryType_LpDdr4;
SDRAM[0].EmcBctSpare4 = 0x7001bc68;
SDRAM[3].EmcBctSpare4 = 0x7001bc68;
";
        let unsigned = from_cfg_unsigned(cfg, DEFAULT_ODMDATA).unwrap();
        assert_eq!(unsigned.len(), BCT_SIZE);
        assert_eq!(u32::from_le_bytes(unsigned[0x0000..0x0004].try_into().unwrap()), 0x200);
        assert_eq!(u32::from_le_bytes(unsigned[0x0004..0x0008].try_into().unwrap()), 0x0e0f);
        assert_eq!(
            u32::from_le_bytes(unsigned[0x0508..0x050c].try_into().unwrap()),
            DEFAULT_ODMDATA
        );
        assert_eq!(
            u32::from_le_bytes(unsigned[0x0530..0x0534].try_into().unwrap()),
            0x0021_0001
        );
        assert_eq!(u32::from_le_bytes(unsigned[0x058c..0x0590].try_into().unwrap()), 3);
        assert_eq!(
            u32::from_le_bytes(unsigned[0x05bc..0x05c0].try_into().unwrap()),
            0x7001_bc68
        );
        assert_eq!(
            u32::from_le_bytes(unsigned[0x1bf4..0x1bf8].try_into().unwrap()),
            0x7001_bc68
        );

        let signed = from_cfg(cfg, DEFAULT_ODMDATA).unwrap();
        assert_eq!(
            &signed[CMAC_OFFSET..CMAC_OFFSET + 16],
            &zero_key_cmac(&signed[SIGN_OFFSET..])
        );
    }

    #[test]
    fn matches_l4t_p3448_cfg_byte_for_byte_if_present() {
        let cfg_path = std::path::Path::new(
            r"C:\Users\Lerou\Documents\jetson\l4t-bsp\Linux_for_Tegra\bootloader\t210ref\BCT\P3448_A00_lpddr4_204Mhz_P987.cfg",
        );
        if !cfg_path.is_file() {
            return;
        }
        let cfg = std::fs::read_to_string(cfg_path).unwrap();
        let signed = from_cfg(&cfg, DEFAULT_ODMDATA).unwrap();
        let hash: String = Sha256::digest(&signed).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash, "f7e6362074bcb8d4408ce2fbf497ceb947b765884b16b69f919bb12fbb84e0ea");
        let cmac_hex: String = signed[CMAC_OFFSET..CMAC_OFFSET + 16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(cmac_hex, "2426d41b06bccd7bd5db29556dcc79c2");
    }
}
