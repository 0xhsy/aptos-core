import numpy as np
import numpy.linalg as la
import scipy as sp

data = [

    # original schedule:
    ["entry_point_Nop", 4103, 1.5, 0.0012, 0, 0],
    ["entry_point_BytesMakeOrChange { data_length: Some(32) }", 3411, 1.5, 0.099, 0.3, 0.474],
    ["entry_point_StepDst", 3270, 1.5, 0.1894, 0.6024, 0.956],
    ["entry_point_Loop { loop_count: Some(100000), loop_type: NoOp }", 28, 1.5, 2400.0128, 0, 0],
    ["entry_point_Loop { loop_count: Some(10000), loop_type: Arithmetic }", 42, 1.5, 1448.024, 0, 0],
    ["entry_point_CreateObjects { num_objects: 10, extra_size: 0 }", 1031, 1.5, 4.8356, 3, 7.54],
    ["entry_point_CreateObjects { num_objects: 10, extra_size: 10240 }", 108, 1.5, 505.4636, 6, 213.54],
    ["entry_point_CreateObjects { num_objects: 100, extra_size: 0 }", 148, 1.5, 47.3516, 30, 75.4],
    ["entry_point_CreateObjects { num_objects: 100, extra_size: 10240 }", 50, 1.5, 629.9516, 60, 2135.4],
    ["entry_point_InitializeVectorPicture { length: 40 }", 2100, 1.5, 2.6054, 0.9, 1.662],
    ["entry_point_VectorPicture { length: 40 }", 3400, 1.5, 0.1048, 0.7422, 1.11],
    ["entry_point_VectorPictureRead { length: 40 }", 3480, 1.5, 0.147, 0.7422, 0],
    ["entry_point_InitializeVectorPicture { length: 30720 }", 31, 1.5, 1438.4294, 0.9, 185.75],
    ["entry_point_VectorPicture { length: 30720 }", 180, 1.5, 0.1048, 55.968, 185.198],
    ["entry_point_VectorPictureRead { length: 30720 }", 205, 1.5, 0.147, 55.968, 0],
    ["entry_point_SmartTablePicture { length: 30720, num_points_per_txn: 200 }", 17.8, 4.254, 1620.5714, 0.7107, 7.712],
    ["entry_point_SmartTablePicture { length: 1048576, num_points_per_txn: 1024 }", 2.75, 19.086, 11947.3918, 0.7107, 34.484],
    ["entry_point_TokenV1MintAndTransferFT", 1719, 1.5, 17.30746, 2.0007, 5.726],
    ["entry_point_TokenV1MintAndTransferNFTSequential", 1150, 1.5, 28.03864, 2.0007, 6.306],
    ["Transfer", 2791, 1.5, 2.249, 0.663, 1.564],
    ["CreateAccount", 2215, 1.5, 3.1536, 0.6, 1.54],
    ["PublishModule", 148, 27.786, 2.8636, 99.9063, 28.616],
]

A = np.array([[row[i] * row[1] for i in range(2, 6)] for row in data])
b = np.array([[20000]]*len(data))

print("LSQ")
x = la.lstsq(A, b)[0]
print(np.matmul(A, x)) 
print(x)
print()

print("LSQ - Constrained")
res = sp.optimize.lsq_linear(A, np.matrix.flatten(b), (0.1, 10))
x = np.array([res.x]).transpose()
print(np.matmul(A, x)) 
print(x)
print((res.cost, res.optimality, res.message))