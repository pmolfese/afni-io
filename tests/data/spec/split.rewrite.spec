
#define the group
	Group = SureFitSubj

#define various States
	StateDef = fiducial
	StateDef = inflated

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = SureFit
	CoordFile = ./lh.fiducial.coord
	TopoFile = ./lh.closed.topo
	SureFitVolParam = ./lh.params
	LocalDomainParent = ./SAME
	LabelDset = ./lh.parc.niml.dset
	NodeMarker = ./lh.marks.niml.dset
	SurfaceState = fiducial
	EmbedDimension = 3
	SurfaceVolume = anat+orig.HEAD
	SurfaceLabel = LHfid
	Hemisphere = B
	DomainGrandParentID = DGP_1
	OriginatorID = ORIG_1
	LocalCurvatureParent = ./SAME

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = SureFit
	CoordFile = ./lh.inflated.coord
	TopoFile = ./lh.closed.topo
	SureFitVolParam = ./lh.params
	LocalDomainParent = ./lh.fiducial.coord
	SurfaceState = inflated
	EmbedDimension = 3
	LocalCurvatureParent = ./lh.fiducial.coord
