#define the group
	Group = SureFitSubj

	StateDef = fiducial
	StateDef = inflated

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = SureFit
	CoordFile = lh.fiducial.coord
	TopoFile = lh.closed.topo
	SureFitVolParam = lh.params
	SurfaceVolume = anat+orig.HEAD
	SurfaceLabel = LHfid
	SurfaceState = fiducial
	LocalDomainParent = SAME
	LabelDset = lh.parc.niml.dset
	NodeMarker = lh.marks.niml.dset
	DomainGrandParentID = DGP_1
	OriginatorID = ORIG_1
	Hemisphere = B
	Anatomical = N # a trailing comment hides the whole line from AFNI

NewSurface
	CoordFile = lh.inflated.coord
	SurfaceState = inflated
	LocalDomainParent = lh.fiducial.coord
